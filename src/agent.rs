//! The target side: dial out, forever, and serve whatever the controller opens.
//!
//! The agent never listens for inbound connections and never learns a secret from
//! the image — its own keypair is generated on first run and its identity is the
//! iroh NodeId derived from it, which is exactly the key the controller approves.
//! That is why there is no application-level handshake here: QUIC has already
//! proved possession of that key by the time a connection exists.

use crate::net::{self, RelayChoice};
use crate::state::load_or_create_key;
use crate::pipe::splice;
use crate::proto::{
    read_msg, recv_file_body, send_file_body, stat_and_hash, write_msg, Ack, ExecFrame,
    PullStart, Request, ALPN, CLOSE_PENDING,
};
use anyhow::{Context, Result};
use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use iroh::{EndpointId, SecretKey};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
/// Retry floor after a *successful* connection, so approval is picked up quickly.
const SHORT_RETRY: Duration = Duration::from_secs(1);
/// A target that is merely unapproved should not hammer the relay while a human
/// gets around to approving it.
const PENDING_RETRY: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const EXEC_CHUNK: usize = 32 * 1024;

pub struct Config {
    pub controller: EndpointId,
    pub key_file: PathBuf,
    pub relay: RelayChoice,
    pub direct: Vec<SocketAddr>,
}

enum Outcome {
    /// The controller turned us away: not approved yet.
    Pending,
    /// We were connected and the connection ended.
    Ended,
}

/// Never returns. A target has no screen and nobody to press a key on it, so
/// every failure here is a sleep and another attempt.
pub async fn run(cfg: Config) -> Result<()> {
    // The agent keeps this across reboots so an approved target stays approved; on
    // a tmpfs initramfs it is regenerated per boot and needs approving again, which
    // is a property of the medium, not of this code.
    let secret = load_or_create_key(&cfg.key_file)?;
    // stdout, not a log line: on a bare target this print is how the operator
    // learns which key to approve, e.g. from a serial console capture.
    println!("agent pubkey: {}", secret.public());
    tracing::info!(controller = %cfg.controller, "egdod agent starting");

    let mut backoff = SHORT_RETRY;
    loop {
        let delay = match session(&secret, &cfg).await {
            Ok(Outcome::Pending) => {
                tracing::warn!(
                    pubkey = %secret.public(),
                    "controller has not approved this agent yet; retrying"
                );
                backoff = SHORT_RETRY;
                PENDING_RETRY
            }
            Ok(Outcome::Ended) => {
                tracing::info!("controller connection ended; redialing");
                backoff = SHORT_RETRY;
                SHORT_RETRY
            }
            Err(e) => {
                tracing::warn!("dial failed: {e:#}");
                let d = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                d
            }
        };
        tokio::time::sleep(delay).await;
    }
}

async fn session(secret: &SecretKey, cfg: &Config) -> Result<Outcome> {
    // No publishing: a target should leave no discoverable record of itself.
    // No resolving either when it was told exactly where to dial, which is what
    // makes a DNS-free target possible (`--direct`, plus an IP-literal `--relay`).
    let lookup = if cfg.direct.is_empty() {
        net::Lookup::Resolve
    } else {
        net::Lookup::None
    };
    let ep = net::bind(Some(secret.clone()), &cfg.relay, lookup, None).await?;
    let addr = net::endpoint_addr(cfg.controller, &cfg.direct, cfg.relay.first_url()?.as_ref());
    let connected = tokio::time::timeout(CONNECT_TIMEOUT, ep.connect(addr, ALPN)).await;
    let outcome = match connected {
        Err(_) => Err(anyhow::anyhow!("connect timed out after {CONNECT_TIMEOUT:?}")),
        // The controller can close before we ever accept a stream, so "not
        // approved" has to be recognised here too or the target loses the only
        // signal it can log about why it is getting nowhere.
        Ok(Err(e)) if closed_as_pending(&e) => Ok(Outcome::Pending),
        Ok(Err(e)) => Err(anyhow::Error::new(e).context("dialling controller")),
        Ok(Ok(conn)) => {
            let (path, remote) = net::session_path_report(&conn);
            tracing::info!(%path, %remote, "connected to controller");
            Ok(serve_connection(conn).await)
        }
    };
    // Closing explicitly keeps the peer from waiting out an idle timeout, and
    // keeps iroh from complaining about a dropped endpoint on every redial.
    ep.close().await;
    outcome
}

async fn serve_connection(conn: Connection) -> Outcome {
    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle(send, recv).await {
                        tracing::warn!("request failed: {e:#}");
                    }
                });
            }
            Err(e) if is_pending_close(&e) => return Outcome::Pending,
            Err(e) => {
                tracing::debug!("connection closed: {e}");
                return Outcome::Ended;
            }
        }
    }
}

fn is_pending_close(e: &ConnectionError) -> bool {
    matches!(e, ConnectionError::ApplicationClosed(close)
        if u64::from(close.error_code) == u64::from(CLOSE_PENDING))
}

fn closed_as_pending(e: &iroh::endpoint::ConnectError) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = source {
        if let Some(ce) = err.downcast_ref::<ConnectionError>() {
            return is_pending_close(ce);
        }
        source = err.source();
    }
    false
}

async fn handle(mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let req: Request = read_msg(&mut recv).await.context("reading request")?;
    match req {
        Request::Exec { argv } => exec(argv, send).await,
        Request::Push {
            path,
            mode,
            len,
            sha256,
        } => {
            let dest = PathBuf::from(&path);
            let result = recv_file_body(&mut recv, &dest, len, sha256, mode).await;
            let ack = match &result {
                Ok(()) => {
                    tracing::info!(%path, len, "received file");
                    Ack::Ok
                }
                Err(e) => Ack::Err(format!("{e:#}")),
            };
            write_msg(&mut send, &ack).await?;
            send.finish()?;
            result
        }
        Request::Pull { path } => {
            let src = PathBuf::from(&path);
            match stat_and_hash(&src).await {
                Err(e) => {
                    write_msg(&mut send, &PullStart::Err(format!("{e:#}"))).await?;
                    send.finish()?;
                    Ok(())
                }
                Ok((mode, len, sha256)) => {
                    write_msg(&mut send, &PullStart::Ok { mode, len, sha256 }).await?;
                    send_file_body(&mut send, &src).await?;
                    send.finish()?;
                    tracing::info!(%path, len, "sent file");
                    Ok(())
                }
            }
        }
        Request::Forward { addr } => match tokio::net::TcpStream::connect(&addr).await {
            Err(e) => {
                write_msg(&mut send, &Ack::Err(format!("connecting to {addr}: {e}"))).await?;
                send.finish()?;
                Ok(())
            }
            Ok(tcp) => {
                write_msg(&mut send, &Ack::Ok).await?;
                let (tcp_r, tcp_w) = tcp.into_split();
                splice(tcp_r, tcp_w, recv, send).await;
                Ok(())
            }
        },
    }
}

/// Runs argv with the agent's own privileges — root when the agent is init, which
/// is the deployment this is for. stdout and stderr stay distinguishable all the
/// way to the controller's own stdout/stderr.
async fn exec(argv: Vec<String>, mut send: SendStream) -> Result<()> {
    let Some((program, args)) = argv.split_first() else {
        write_msg(&mut send, &ExecFrame::Failed("empty argv".into())).await?;
        send.finish()?;
        return Ok(());
    };
    tracing::info!(?argv, "exec");
    let child = tokio::process::Command::new(program)
        .args(args)
        // No stdin: v0 exec is fire-and-collect, and a command that blocks on a
        // terminal that does not exist would hang forever.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            write_msg(&mut send, &ExecFrame::Failed(format!("spawning {program}: {e}"))).await?;
            send.finish()?;
            return Ok(());
        }
    };
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");

    // One writer owns the stream; the two readers feed it, which is what keeps
    // stdout and stderr framed separately instead of interleaved into one blob.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ExecFrame>(16);
    let out_tx = tx.clone();
    let out = tokio::spawn(async move { pump_frames(&mut stdout, &out_tx, true).await });
    let err_tx = tx.clone();
    let err = tokio::spawn(async move { pump_frames(&mut stderr, &err_tx, false).await });
    drop(tx);

    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if let Err(e) = write_msg(&mut send, &frame).await {
                tracing::warn!("exec output stream broke: {e:#}");
                return None;
            }
        }
        Some(send)
    });

    let status = child.wait().await.context("waiting for child")?;
    // Bounded: a command that forks a process holding the same pipes would
    // otherwise keep them open forever, and the exit status would never arrive.
    let drain = Duration::from_secs(2);
    let _ = tokio::time::timeout(drain, out).await;
    let _ = tokio::time::timeout(drain, err).await;
    if let Ok(Some(mut send)) = writer.await {
        write_msg(&mut send, &ExecFrame::Exit(exit_code(&status))).await?;
        send.finish()?;
    }
    Ok(())
}

async fn pump_frames<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    tx: &tokio::sync::mpsc::Sender<ExecFrame>,
    is_stdout: bool,
) {
    let mut buf = vec![0u8; EXEC_CHUNK];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let chunk = buf[..n].to_vec();
                let frame = if is_stdout {
                    ExecFrame::Stdout(chunk)
                } else {
                    ExecFrame::Stderr(chunk)
                };
                if tx.send(frame).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn exit_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_key_is_stable_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/agent.key");
        let a = load_or_create_key(&path).unwrap();
        let b = load_or_create_key(&path).unwrap();
        assert_eq!(a.public(), b.public());
    }
}
