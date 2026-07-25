//! The controller: one long-lived `serve` process, and short-lived commands that
//! reach it through a unix socket in the state directory.
//!
//! `serve` is deliberately a dumb proxy. It authorises the agent, opens a QUIC
//! stream, and then copies bytes; the request protocol itself is implemented once
//! on the command side and once in the agent. That is what keeps `ssh` a
//! composition of the three primitives rather than a fourth primitive.

use crate::net::{self, Lookup, ProbeMode, RelayChoice};
use crate::pipe::splice;
use crate::proto::{
    finish, hex, read_msg, read_msg_opt, recv_file_body, send_file_body, set_mode, stat_and_hash,
    write_msg, Ack, ExecFrame, PullStart, Request, Route, RouteReply, ALPN, CLOSE_PENDING,
    PROBE_ALPN, PROBE_PING, PROBE_PONG,
};
use crate::state::{now_unix, SessionInfo, StateDir, Status};
use anyhow::{bail, Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::EndpointId;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::Mutex;

pub struct ServeConfig {
    pub state: StateDir,
    pub relay: RelayChoice,
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    /// Consecutive canary failures tolerated before the endpoint is rebuilt. The
    /// prior art's undialable-controller flake cleared on exactly that.
    pub restart_after: u32,
    /// Pin the UDP socket, so agents can be given a fixed --direct address that
    /// survives a controller restart.
    pub bind_addr: Option<SocketAddr>,
}

#[derive(Clone)]
struct Session {
    conn: Connection,
    /// Distinguishes two connections from the same agent key, which is what a
    /// redial after an unnoticed drop produces.
    id: usize,
    since: u64,
}

type Sessions = Arc<Mutex<HashMap<EndpointId, Session>>>;

/// Owns status.json. Every mutation rewrites the file, so an external program
/// polling it sees the controller's real opinion of itself and not a cached one.
struct StatusFile {
    state: StateDir,
    inner: Mutex<Status>,
}

impl StatusFile {
    async fn update(&self, f: impl FnOnce(&mut Status)) {
        let mut guard = self.inner.lock().await;
        f(&mut guard);
        guard.updated_unix = now_unix();
        if let Err(e) = self.state.write_status(&guard) {
            tracing::warn!("writing status.json: {e:#}");
        }
    }
}

pub async fn serve(cfg: ServeConfig) -> Result<()> {
    cfg.state.ensure()?;
    let secret = cfg.state.load_key()?;
    let node_id = secret.public();

    let sock = cfg.state.sock_path();
    // Two controllers sharing one state directory would share one identity and
    // fight over the socket, so the live one is asked first whether it is alive.
    if UnixStream::connect(&sock).await.is_ok() {
        bail!(
            "another controller is already serving {}",
            cfg.state.root().display()
        );
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("binding control socket {}", sock.display()))?;

    let probe_mode = if cfg.relay.enabled() {
        ProbeMode::Discovery
    } else {
        ProbeMode::Direct
    };
    let status = Arc::new(StatusFile {
        state: cfg.state.clone(),
        inner: Mutex::new(Status {
            node_id: node_id.to_string(),
            pid: std::process::id(),
            updated_unix: now_unix(),
            direct_addrs: Vec::new(),
            dialable: None,
            probe_mode,
            last_probe_ok_unix: None,
            last_probe_path: None,
            last_probe_error: None,
            consecutive_probe_failures: 0,
            endpoint_restarts: 0,
            sessions: Vec::new(),
        }),
    });
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));

    println!("controller node id: {node_id}");
    println!("state dir: {}", cfg.state.root().display());

    let mut first = true;
    loop {
        // The first bind must fail loudly — a bad --bind is a misconfiguration,
        // and retrying it forever would leave commands hanging on a socket nobody
        // is reading. A *re*bind fails exactly when the restart is most needed
        // (the network went away), so that one retries.
        let ep = loop {
            match net::bind(Some(secret.clone()), &cfg.relay, Lookup::Both, cfg.bind_addr).await {
                Ok(ep) => break ep,
                Err(e) if first => return Err(e),
                Err(e) => {
                    tracing::error!("cannot rebind an endpoint: {e:#}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        };
        first = false;
        let addrs: Vec<SocketAddr> = ep.addr().ip_addrs().copied().collect();
        status
            .update(|s| s.direct_addrs = addrs.iter().map(|a| a.to_string()).collect())
            .await;
        for a in &addrs {
            tracing::info!(addr = %a, "listening (direct)");
        }

        let router = Router::builder(ep.clone())
            .accept(
                ALPN,
                Acceptor {
                    state: cfg.state.clone(),
                    sessions: sessions.clone(),
                    status: status.clone(),
                },
            )
            .accept(PROBE_ALPN, ProbeResponder)
            .spawn();

        let (restart_tx, restart_rx) = tokio::sync::oneshot::channel();
        let canary = tokio::spawn(canary(
            node_id,
            probe_mode,
            addrs,
            cfg.relay.clone(),
            cfg.probe_interval,
            cfg.probe_timeout,
            cfg.restart_after,
            status.clone(),
            sessions.clone(),
            restart_tx,
        ));

        tokio::select! {
            r = accept_local(&listener, sessions.clone()) => {
                canary.abort();
                let _ = router.shutdown().await;
                ep.close().await;
                return r;
            }
            _ = restart_rx => {
                tracing::error!("rebuilding the iroh endpoint: no dialer has been able to reach us");
            }
        }

        canary.abort();
        let _ = router.shutdown().await;
        ep.close().await;
        sessions.lock().await.clear();
        status
            .update(|s| {
                s.endpoint_restarts += 1;
                s.sessions.clear();
                s.consecutive_probe_failures = 0;
            })
            .await;
    }
}

#[derive(Debug, Clone)]
struct ProbeResponder;

impl ProtocolHandler for ProbeResponder {
    /// Answers any dialer, including one that has never been approved: the canary
    /// has to look like a stranger to be worth anything, and the answer is a
    /// constant that reveals nothing the dialer did not already know.
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        if let Ok((mut send, mut recv)) = conn.accept_bi().await {
            if recv.read_to_end(PROBE_PING.len()).await.as_deref() == Ok(PROBE_PING) {
                let _ = send.write_all(PROBE_PONG).await;
                let _ = send.finish();
                // The dialer reads to EOF; give it a moment before the connection goes.
                let _ = tokio::time::timeout(Duration::from_secs(1), conn.closed()).await;
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Acceptor {
    state: StateDir,
    sessions: Sessions,
    status: Arc<StatusFile>,
}

impl std::fmt::Debug for Acceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Acceptor")
    }
}

impl ProtocolHandler for Acceptor {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = conn.remote_id();
        let approved = match self.state.approved() {
            Ok(set) => set.contains(&peer),
            Err(e) => {
                tracing::error!("cannot read the approved list: {e:#}");
                false
            }
        };
        if !approved {
            if let Err(e) = self.state.record_pending(&peer) {
                tracing::warn!("recording pending agent: {e:#}");
            }
            tracing::warn!(agent = %peer, "unapproved agent held pending; nothing served");
            conn.close(CLOSE_PENDING.into(), b"pending approval");
            return Ok(());
        }

        let (path, remote) = net::session_path_report(&conn);
        tracing::info!(agent = %peer, %path, %remote, "agent connected");
        let id = conn.stable_id();
        self.sessions.lock().await.insert(
            peer,
            Session {
                conn: conn.clone(),
                id,
                since: now_unix(),
            },
        );
        publish_sessions(&self.sessions, &self.status).await;

        // Republish as soon as the path changes rather than waiting for the next
        // canary tick, so `status` cannot describe a session that holepunched (or
        // fell back to the relay) minutes ago as still being on its first path.
        let sessions = self.sessions.clone();
        let status = self.status.clone();
        let watched = conn.clone();
        tokio::spawn(async move {
            let mut changes = net::path_changes(watched);
            while let Some((was, now, remote)) = changes.recv().await {
                tracing::info!(agent = %peer, %was, %now, %remote, "session path changed");
                publish_sessions(&sessions, &status).await;
            }
        });

        let sessions = self.sessions.clone();
        let status = self.status.clone();
        tokio::spawn(async move {
            conn.closed().await;
            {
                // An agent that redialled before we noticed this connection die
                // already holds the slot; forgetting by key alone would evict the
                // live session and leave a connected agent unreachable.
                let mut map = sessions.lock().await;
                match map.get(&peer) {
                    Some(s) if s.id == id => {
                        map.remove(&peer);
                    }
                    _ => return,
                }
            }
            publish_sessions(&sessions, &status).await;
            tracing::info!(agent = %peer, "agent disconnected");
        });
        Ok(())
    }
}

async fn publish_sessions(sessions: &Sessions, status: &StatusFile) {
    let list: Vec<SessionInfo> = sessions
        .lock()
        .await
        .iter()
        .map(|(id, s)| {
            let (path, remote) = net::session_path_report(&s.conn);
            SessionInfo {
                agent: id.to_string(),
                path,
                remote,
                since_unix: s.since,
            }
        })
        .collect();
    status.update(|s| s.sessions = list).await;
}

#[allow(clippy::too_many_arguments)]
async fn canary(
    node_id: EndpointId,
    mode: ProbeMode,
    addrs: Vec<SocketAddr>,
    relay: RelayChoice,
    interval: Duration,
    timeout: Duration,
    restart_after: u32,
    status: Arc<StatusFile>,
    sessions: Sessions,
    restart: tokio::sync::oneshot::Sender<()>,
) {
    let mut failures = 0u32;
    loop {
        // A session that started on the relay and later holepunched to a direct
        // path would otherwise keep its stale label in status.json.
        publish_sessions(&sessions, &status).await;
        match net::probe(node_id, mode, &addrs, &relay, timeout).await {
            Ok(path) => {
                failures = 0;
                tracing::info!(%path, ?mode, "reachability confirmed: a stranger can dial us");
                status
                    .update(|s| {
                        s.dialable = Some(true);
                        s.last_probe_ok_unix = Some(now_unix());
                        s.last_probe_path = Some(path);
                        s.last_probe_error = None;
                        s.consecutive_probe_failures = 0;
                    })
                    .await;
            }
            Err(e) => {
                failures += 1;
                // Loud on purpose: this is the state where the controller looks
                // healthy while a target boots, dials, fails, and cannot say so.
                tracing::error!(
                    failures,
                    "UNDIALABLE: cannot reach our own node id ({e:#}). \
                     A target booting now would not find us."
                );
                let msg = format!("{e:#}");
                status
                    .update(|s| {
                        s.dialable = Some(false);
                        s.last_probe_error = Some(msg);
                        s.consecutive_probe_failures = failures;
                    })
                    .await;
                if failures >= restart_after {
                    let _ = restart.send(());
                    return;
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}

async fn accept_local(listener: &UnixListener, sessions: Sessions) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await.context("accepting control socket")?;
        let sessions = sessions.clone();
        tokio::spawn(async move {
            if let Err(e) = route_local(stream, sessions).await {
                tracing::warn!("control connection failed: {e:#}");
            }
        });
    }
}

async fn route_local(stream: UnixStream, sessions: Sessions) -> Result<()> {
    let (mut r, mut w) = stream.into_split();
    let route: Route = read_msg(&mut r).await.context("reading route request")?;
    let picked = pick(&sessions, &route.agent).await;
    let (id, conn) = match picked {
        Ok(v) => v,
        Err(e) => {
            write_msg(&mut w, &RouteReply::Err(format!("{e:#}"))).await?;
            return Ok(());
        }
    };
    let (path, remote) = net::session_path_report(&conn);
    let (qs, qr) = match conn.open_bi().await {
        Ok(v) => v,
        Err(e) => {
            write_msg(&mut w, &RouteReply::Err(format!("opening stream: {e}"))).await?;
            return Ok(());
        }
    };
    write_msg(
        &mut w,
        &RouteReply::Ok {
            agent: id.to_string(),
            path,
            remote: remote.clone(),
        },
    )
    .await?;
    tracing::info!(agent = %id, %path, %remote, "routed a session");
    splice(r, w, qr, qs).await;
    Ok(())
}

async fn pick(sessions: &Sessions, selector: &str) -> Result<(EndpointId, Connection)> {
    let map = sessions.lock().await;
    let sel = selector.trim().to_lowercase();
    let mut matches: Vec<(EndpointId, Connection)> = map
        .iter()
        .filter(|(id, _)| sel.is_empty() || sel == "any" || id.to_string().starts_with(&sel))
        .map(|(id, s)| (*id, s.conn.clone()))
        .collect();
    match matches.len() {
        0 if map.is_empty() => bail!("no agent is connected"),
        0 => bail!("no connected agent matches {selector:?}"),
        1 => Ok(matches.remove(0)),
        n => bail!("{n} connected agents match {selector:?}; use a longer prefix"),
    }
}

// ---------------------------------------------------------------------------
// Command side: short-lived processes that talk to `serve` over the unix socket.
// ---------------------------------------------------------------------------

struct Routed {
    read: tokio::net::unix::OwnedReadHalf,
    write: tokio::net::unix::OwnedWriteHalf,
}

/// Opens one request stream to an agent, and reports the path it travels on —
/// every session, every time, on stderr so it never pollutes command output.
async fn route(state: &StateDir, agent: &str) -> Result<Routed> {
    let sock = state.sock_path();
    let stream = UnixStream::connect(&sock).await.with_context(|| {
        format!(
            "connecting to {} — is `egdod controller serve` running?",
            sock.display()
        )
    })?;
    let (mut read, mut write) = stream.into_split();
    write_msg(
        &mut write,
        &Route {
            agent: agent.to_string(),
        },
    )
    .await?;
    match read_msg::<_, RouteReply>(&mut read).await? {
        RouteReply::Err(e) => bail!("{e}"),
        RouteReply::Ok {
            agent,
            path,
            remote,
        } => {
            eprintln!("egdod: session with {agent} via {path} ({remote})");
            Ok(Routed { read, write })
        }
    }
}

/// Runs argv on the target, writing its output to `out` and `err` as it arrives,
/// and returns the target's exit status.
///
/// The command and the ssh recipe share this one implementation; they differ only
/// in the sinks they hand it (a terminal, or a buffer).
pub async fn exec_into<O, E>(
    state: &StateDir,
    agent: &str,
    argv: Vec<String>,
    out: &mut O,
    err: &mut E,
) -> Result<i32>
where
    O: tokio::io::AsyncWrite + Unpin,
    E: tokio::io::AsyncWrite + Unpin,
{
    let mut r = route(state, agent).await?;
    write_msg(&mut r.write, &Request::Exec { argv }).await?;
    finish(&mut r.write).await?;
    let mut code = None;
    let mut truncated = false;
    while let Some(frame) = read_msg_opt::<_, ExecFrame>(&mut r.read).await? {
        match frame {
            ExecFrame::Stdout(b) => out.write_all(&b).await?,
            ExecFrame::Stderr(b) => err.write_all(&b).await?,
            ExecFrame::Exit(c) => code = Some(c),
            ExecFrame::Failed(e) => bail!("exec failed on the target: {e}"),
            ExecFrame::Truncated => truncated = true,
        }
    }
    out.flush().await?;
    err.flush().await?;
    // Loud, because the exit status that follows is about the command and says
    // nothing about whether its output arrived whole.
    if truncated {
        tracing::warn!("the target cut this command's output off; what you have above is a prefix");
    }
    code.context("agent closed the stream without reporting an exit status")
}

/// Returns the target's exit status so the caller can exit with it.
pub async fn exec(state: &StateDir, agent: &str, argv: Vec<String>) -> Result<i32> {
    let (mut out, mut err) = (tokio::io::stdout(), tokio::io::stderr());
    exec_into(state, agent, argv, &mut out, &mut err).await
}

/// Sends a local file, returning what the target now holds.
pub async fn push_file(
    state: &StateDir,
    agent: &str,
    local: &Path,
    remote: &str,
) -> Result<(u64, [u8; 32])> {
    let (mode, len, sha256) = stat_and_hash(local).await?;
    let mut r = route(state, agent).await?;
    write_msg(
        &mut r.write,
        &Request::Push {
            path: remote.to_string(),
            mode,
            len,
            sha256,
        },
    )
    .await?;
    send_file_body(&mut r.write, local).await?;
    finish(&mut r.write).await?;
    match read_msg::<_, Ack>(&mut r.read).await? {
        Ack::Ok => Ok((len, sha256)),
        Ack::Err(e) => bail!("agent rejected the file: {e}"),
    }
}

pub async fn push(state: &StateDir, agent: &str, local: &Path, remote: &str) -> Result<()> {
    let (len, sha256) = push_file(state, agent, local, remote).await?;
    println!(
        "pushed {} -> {remote} ({len} bytes, sha256 {})",
        local.display(),
        hex(&sha256)
    );
    Ok(())
}

/// What a `pull` found. `Missing` means the file is not there and nothing else:
/// the ssh recipe rewrites what it reads, so "I could not read it" must never
/// collapse into "it is empty".
pub enum Pulled {
    Fetched { len: u64, sha256: [u8; 32] },
    Missing,
}

pub async fn pull_file(
    state: &StateDir,
    agent: &str,
    remote: &str,
    local: &Path,
) -> Result<Pulled> {
    let mut r = route(state, agent).await?;
    write_msg(
        &mut r.write,
        &Request::Pull {
            path: remote.to_string(),
        },
    )
    .await?;
    finish(&mut r.write).await?;
    match read_msg::<_, PullStart>(&mut r.read).await? {
        PullStart::Missing => Ok(Pulled::Missing),
        PullStart::Err(e) => bail!("agent could not read {remote}: {e}"),
        PullStart::Ok { mode, len, sha256 } => {
            recv_file_body(&mut r.read, local, len, sha256, mode).await?;
            Ok(Pulled::Fetched { len, sha256 })
        }
    }
}

pub async fn pull(state: &StateDir, agent: &str, remote: &str, local: &Path) -> Result<()> {
    match pull_file(state, agent, remote, local).await? {
        Pulled::Missing => bail!("the target has no {remote}"),
        Pulled::Fetched { len, sha256 } => {
            println!(
                "pulled {remote} -> {} ({len} bytes, sha256 {})",
                local.display(),
                hex(&sha256)
            );
            Ok(())
        }
    }
}

/// Binds the local listener and serves it until the returned handle is aborted.
/// The ssh recipe uses this directly, so the recipe really is the `forward`
/// primitive rather than a second implementation of it.
pub async fn forward_listener(
    state: &StateDir,
    agent: &str,
    local: SocketAddr,
    remote: String,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(local)
        .await
        .with_context(|| format!("binding {local}"))?;
    let bound = listener.local_addr()?;
    let state = state.clone();
    let agent = agent.to_string();
    let handle = tokio::spawn(async move {
        loop {
            let (tcp, _peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    // A listener error is usually resource exhaustion; retrying
                    // flat out would spin a CPU on a controller that may be a phone.
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            let state = state.clone();
            let agent = agent.clone();
            let remote = remote.clone();
            tokio::spawn(async move {
                if let Err(e) = forward_one(&state, &agent, &remote, tcp).await {
                    tracing::warn!("forwarded connection failed: {e:#}");
                }
            });
        }
    });
    Ok((bound, handle))
}

async fn forward_one(
    state: &StateDir,
    agent: &str,
    remote: &str,
    tcp: tokio::net::TcpStream,
) -> Result<()> {
    let mut r = route(state, agent).await?;
    write_msg(
        &mut r.write,
        &Request::Forward {
            addr: remote.to_string(),
        },
    )
    .await?;
    match read_msg::<_, Ack>(&mut r.read).await? {
        Ack::Err(e) => bail!("agent could not connect to {remote}: {e}"),
        Ack::Ok => {
            let (tcp_r, tcp_w) = tcp.into_split();
            splice(tcp_r, tcp_w, r.read, r.write).await;
            Ok(())
        }
    }
}

pub async fn forward(
    state: &StateDir,
    agent: &str,
    local_port: u16,
    remote: String,
) -> Result<()> {
    let local: SocketAddr = ([127, 0, 0, 1], local_port).into();
    let (bound, handle) = forward_listener(state, agent, local, remote.clone()).await?;
    println!("forwarding {bound} -> {remote} on the target (ctrl-c to stop)");
    let _ = tokio::signal::ctrl_c().await;
    handle.abort();
    Ok(())
}

pub fn init(state: &StateDir) -> Result<()> {
    let secret = state.init_key()?;
    println!("{}", secret.public());
    eprintln!(
        "egdod: controller key at {} — bake the node id above into images; it is public",
        state.key_path().display()
    );
    Ok(())
}

pub fn pending(state: &StateDir, json: bool) -> Result<()> {
    let list = state.pending()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if list.is_empty() {
        println!("no agents pending approval");
    }
    for p in list {
        println!(
            "{}  attempts={} first_seen={} last_seen={}",
            p.pubkey, p.attempts, p.first_seen_unix, p.last_seen_unix
        );
    }
    Ok(())
}

pub fn approve(state: &StateDir, key: &str) -> Result<()> {
    let key = crate::proto::parse_pubkey(key)?;
    if state.approve(&key)? {
        println!("approved {key}");
    } else {
        println!("{key} was already approved");
    }
    Ok(())
}

/// Exit code when the controller is up but nobody can dial it. The spec wants
/// this state *exposed*, and the intended operator is a program on a handset, so
/// it has to be legible without parsing prose. A dead or never-probed controller
/// is the same answer: do not ship an image pointed at this node id yet.
pub const EXIT_UNDIALABLE: i32 = 2;

/// Returns the process exit code, so an unattended controller can be checked
/// with `egdod controller status >/dev/null || wait`.
pub fn status(state: &StateDir, json: bool) -> Result<i32> {
    let s = state.read_status()?;
    let code = if s.dialable == Some(true) && s.writer_alive() {
        0
    } else {
        EXIT_UNDIALABLE
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(code);
    }
    println!("node id:   {}", s.node_id);
    println!(
        "dialable:  {}",
        match s.dialable {
            Some(true) => "yes".to_string(),
            Some(false) => format!(
                "NO — {}",
                s.last_probe_error.as_deref().unwrap_or("unknown")
            ),
            None => "not probed yet".to_string(),
        }
    );
    println!(
        "probe:     {:?} mode, last ok {}",
        s.probe_mode,
        s.last_probe_ok_unix
            .map(|t| t.to_string())
            .unwrap_or_else(|| "never".into())
    );
    println!("restarts:  {}", s.endpoint_restarts);
    if !s.writer_alive() {
        println!("WARNING:   the controller that wrote this (pid {}) is gone; everything above is its last opinion, not the current one", s.pid);
    }
    for sess in &s.sessions {
        println!("agent:     {} via {} ({})", sess.agent, sess.path, sess.remote);
    }
    if s.sessions.is_empty() {
        println!("agent:     none connected");
    }
    Ok(code)
}

/// Runs argv on the target and collects its output instead of streaming it to a
/// terminal. Used by the ssh recipe, whose steps are decisions, not output.
pub async fn exec_capture(
    state: &StateDir,
    agent: &str,
    argv: Vec<String>,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = exec_into(state, agent, argv, &mut out, &mut err).await?;
    Ok((code, out, err))
}

/// Pulls a small file into memory, staging it through `scratch` because the copy
/// primitive verifies the digest against a written file rather than a buffer.
///
/// `Ok(None)` means the target does not have the file, and nothing else: a
/// transport failure is an error, because the ssh recipe rewrites what it reads.
pub async fn pull_bytes(
    state: &StateDir,
    agent: &str,
    remote: &str,
    scratch: &Path,
) -> Result<Option<Vec<u8>>> {
    match pull_file(state, agent, remote, scratch).await? {
        Pulled::Missing => Ok(None),
        Pulled::Fetched { .. } => Ok(Some(tokio::fs::read(scratch).await?)),
    }
}

/// Pushes bytes produced locally rather than read from a file, staging them
/// through `scratch` so this is the same copy primitive and not a second one.
pub async fn push_bytes(
    state: &StateDir,
    agent: &str,
    remote: &str,
    body: &[u8],
    mode: u32,
    scratch: &Path,
) -> Result<()> {
    tokio::fs::write(scratch, body).await?;
    set_mode(scratch, mode).await?;
    push_file(state, agent, scratch, remote).await?;
    Ok(())
}
