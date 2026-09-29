use crate::net::{self, RelayChoice};
use crate::state::{load_or_create_key, OnUnusable};
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const SHORT_RETRY: Duration = Duration::from_secs(1);
const PENDING_RETRY: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const EXEC_CHUNK: usize = 32 * 1024;
const DRAIN_IDLE: Duration = Duration::from_secs(2);
const DRAIN_CAP: Duration = Duration::from_secs(15);

pub struct Config {
    pub controller: EndpointId,
    pub key_file: PathBuf,
    pub relay: RelayChoice,
    pub direct: Vec<SocketAddr>,
}

enum Outcome {
    Pending,
    Ended(ConnectionError),
}

pub async fn run(cfg: Config) -> Result<()> {
    tracing::info!(controller = %cfg.controller, "egdod agent starting");

    let secret = loop {
        match load_or_create_key(&cfg.key_file, OnUnusable::Replace) {
            Ok(s) => break s,
            Err(e) => {
                tracing::error!("cannot load or create the agent key: {e:#}");
                tokio::time::sleep(MAX_BACKOFF).await;
            }
        }
    };
    println!("agent pubkey: {}", secret.public());

    let mut backoff = SHORT_RETRY;
    let mut attempt = 0u64;
    loop {
        attempt += 1;
        report(format!("dial {attempt}: controller {} via {}", cfg.controller, route(&cfg)));
        let delay = match session(&secret, &cfg).await {
            Ok(Outcome::Pending) => {
                report(format!(
                    "waiting for approval: the controller has not approved {} yet",
                    secret.public()
                ));
                backoff = SHORT_RETRY;
                PENDING_RETRY
            }
            Ok(Outcome::Ended(e)) => {
                report(format!("controller connection ended ({e}); redialing"));
                backoff = SHORT_RETRY;
                SHORT_RETRY
            }
            Err(e) => {
                report(format!("dial failed: {e:#}"));
                let d = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                d
            }
        };
        tokio::time::sleep(delay).await;
    }
}

fn report(line: String) {
    eprintln!("egdod: {line}");
}

fn route(cfg: &Config) -> String {
    let relay = match &cfg.relay {
        RelayChoice::N0 => "the public relays".to_string(),
        RelayChoice::Disabled => "no relay".to_string(),
        RelayChoice::Urls(urls) => urls.join(", "),
    };
    if cfg.direct.is_empty() {
        format!("{relay}, address found by DNS")
    } else {
        let direct: Vec<String> = cfg.direct.iter().map(|a| a.to_string()).collect();
        format!("{} directly, {relay}", direct.join(", "))
    }
}

async fn session(secret: &SecretKey, cfg: &Config) -> Result<Outcome> {
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
        Ok(Err(e)) if closed_as_pending(&e) => Ok(Outcome::Pending),
        Ok(Err(e)) => Err(anyhow::Error::new(e).context("dialling controller")),
        Ok(Ok(conn)) => {
            let (path, remote) = net::session_path_report(&conn);
            report(format!("connected to controller via {path} ({remote})"));
            Ok(serve_connection(conn).await)
        }
    };
    ep.close().await;
    outcome
}

async fn serve_connection(conn: Connection) -> Outcome {
    let mut changes = net::path_changes(conn.clone());
    tokio::spawn(async move {
        while let Some((was, now, remote)) = changes.recv().await {
            report(format!("path changed: {was} -> {now} ({remote})"));
        }
    });
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
            Err(e) => return Outcome::Ended(e),
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
            let result =
                recv_file_body(&mut recv, &dest, len, sha256, mode, u64::MAX).await;
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
                    let reply = match e.downcast_ref::<std::io::Error>() {
                        Some(io) if io.kind() == std::io::ErrorKind::NotFound => PullStart::Missing,
                        _ => PullStart::Err(format!("{e:#}")),
                    };
                    write_msg(&mut send, &reply).await?;
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

async fn exec(argv: Vec<String>, mut send: SendStream) -> Result<()> {
    let Some((program, args)) = argv.split_first() else {
        write_msg(&mut send, &ExecFrame::Failed("empty argv".into())).await?;
        send.finish()?;
        return Ok(());
    };
    tracing::info!(?argv, "exec");
    let child = tokio::process::Command::new(program)
        .args(args)
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

    let (tx, mut rx) = tokio::sync::mpsc::channel::<ExecFrame>(16);
    let out_tx = tx.clone();
    let out = tokio::spawn(async move { pump_frames(&mut stdout, &out_tx, true).await });
    let err_tx = tx.clone();
    let err = tokio::spawn(async move { pump_frames(&mut stderr, &err_tx, false).await });
    drop(tx);

    let progress = Arc::new(Progress::default());
    let mine = progress.clone();
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            mine.sending.store(true, Ordering::Relaxed);
            let r = write_msg(&mut send, &frame).await;
            mine.sending.store(false, Ordering::Relaxed);
            if let Err(e) = r {
                tracing::warn!("exec output stream broke: {e:#}");
                return None;
            }
            mine.frames.fetch_add(1, Ordering::Relaxed);
        }
        Some(send)
    });

    let status = child.wait().await.context("waiting for child")?;
    let cut = drain_output(out, err, &progress).await;
    if let Ok(Ok(Some(mut send))) = tokio::time::timeout(DRAIN_CAP, writer).await {
        if cut {
            write_msg(&mut send, &ExecFrame::Truncated).await?;
        }
        write_msg(&mut send, &ExecFrame::Exit(exit_code(&status))).await?;
        send.finish()?;
    }
    Ok(())
}

#[derive(Default)]
struct Progress {
    frames: AtomicU64,
    sending: std::sync::atomic::AtomicBool,
}

async fn drain_output(
    out: tokio::task::JoinHandle<()>,
    err: tokio::task::JoinHandle<()>,
    progress: &Progress,
) -> bool {
    let aborts = [out.abort_handle(), err.abort_handle()];
    let mut both = tokio::spawn(async move {
        let _ = tokio::join!(out, err);
    });
    let give_up = tokio::time::Instant::now() + DRAIN_CAP;
    loop {
        let before = progress.frames.load(Ordering::Relaxed);
        if tokio::time::timeout(DRAIN_IDLE, &mut both).await.is_ok() {
            return false;
        }
        let idle = progress.frames.load(Ordering::Relaxed) == before
            && !progress.sending.load(Ordering::Relaxed);
        if idle || tokio::time::Instant::now() >= give_up {
            both.abort();
            for a in aborts {
                a.abort();
            }
            return true;
        }
    }
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
        let a = load_or_create_key(&path, OnUnusable::Replace).unwrap();
        let b = load_or_create_key(&path, OnUnusable::Replace).unwrap();
        assert_eq!(a.public(), b.public());
    }
}
