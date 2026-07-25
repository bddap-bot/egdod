//! The long-lived controller: owns the NodeId baked into target images, gates
//! agents on approval, bridges local CLI sessions onto agent connections, and
//! watches its own dialability.

use crate::{classify_path, load_or_create_secret_key, ALPN, CANARY_ALPN};
use anyhow::{Context, Result};
use iroh::endpoint::{presets, Connection};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, PublicKey, RelayMode, RelayUrl, SecretKey};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const CANARY_INTERVAL: Duration = Duration::from_secs(30);
const CANARY_TIMEOUT: Duration = Duration::from_secs(15);

pub struct ServeOpts {
    pub state_dir: PathBuf,
    pub relay: Option<RelayUrl>,
    pub no_relay: bool,
}

pub fn relay_mode(relay: &Option<RelayUrl>, no_relay: bool) -> RelayMode {
    if no_relay {
        RelayMode::Disabled
    } else if let Some(url) = relay {
        RelayMode::custom([url.clone()])
    } else {
        RelayMode::Default
    }
}

#[derive(Clone, Debug)]
struct AgentHandler {
    state_dir: Arc<PathBuf>,
    agents: Agents,
}

impl ProtocolHandler for AgentHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.register(connection).await {
                tracing::warn!("agent registration failed: {e:#}");
            }
        });
        Ok(())
    }
}

impl AgentHandler {
    async fn register(&self, conn: Connection) -> Result<()> {
        let peer = conn.remote_id();
        if !is_approved(&self.state_dir, &peer) {
            record_pending(&self.state_dir, &peer)?;
            println!(
                "egdod serve: PENDING agent {peer} — approve with: egdod controller approve {peer}"
            );
            // The agent gets nothing until approved; it redials on its own
            // backoff and will be re-checked against the approved set then.
            conn.close(1u32.into(), b"pending approval");
            return Ok(());
        }
        // Approval may race a recorded pending entry; the approved set wins.
        let _ = std::fs::remove_file(self.state_dir.join("pending").join(peer.to_string()));
        println!(
            "egdod serve: agent {peer} connected — path: {}",
            classify_path(&conn)
        );
        // A redial replaces the registry entry; the evicted connection's
        // cleanup must not remove the NEW registration when it dies.
        let id = conn.stable_id();
        self.agents.lock().await.insert(peer, (id, conn.clone()));
        let agents = self.agents.clone();
        tokio::spawn(async move {
            conn.closed().await;
            let mut g = agents.lock().await;
            if g.get(&peer).is_some_and(|(stored, _)| *stored == id) {
                g.remove(&peer);
                println!("egdod serve: agent {peer} disconnected");
            }
        });
        Ok(())
    }
}

/// The canary has its own ALPN and is closed immediately: it exists only to
/// prove the controller is dialable by bare NodeId, not to be served.
#[derive(Clone, Debug)]
struct CanaryHandler;

impl ProtocolHandler for CanaryHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        connection.close(0u32.into(), b"canary ok");
        Ok(())
    }
}

pub async fn run(opts: ServeOpts) -> Result<()> {
    let running = bind(opts).await?;
    println!("egdod controller NodeId: {}", running.endpoint.id());
    for a in running.endpoint.addr().ip_addrs() {
        println!("egdod controller direct addr: {a}");
    }
    if let Some(url) = running.endpoint.addr().relay_urls().next() {
        println!("egdod controller relay: {url}");
    }
    tokio::spawn(canary_loop(
        running.relay_mode.clone(),
        running.state_dir.clone(),
        running.endpoint.id(),
    ));
    bridge_listener(running.state_dir.clone(), running.agents).await
}

pub type Agents = Arc<tokio::sync::Mutex<HashMap<PublicKey, (usize, Connection)>>>;

/// Everything `run` sets up, minus the parts a test wants to skip (canary)
/// or drive itself (the bridge listener).
pub struct Running {
    pub endpoint: Endpoint,
    pub agents: Agents,
    pub state_dir: PathBuf,
    pub relay_mode: RelayMode,
    _router: Router,
}

pub async fn bind(opts: ServeOpts) -> Result<Running> {
    std::fs::create_dir_all(opts.state_dir.join("approved"))?;
    std::fs::create_dir_all(opts.state_dir.join("pending"))?;
    let key_path = opts.state_dir.join("controller.key");
    let key = load_or_create_secret_key(&key_path)
        .with_context(|| format!("loading controller key {}", key_path.display()))?;

    let mode = relay_mode(&opts.relay, opts.no_relay);
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .relay_mode(mode.clone())
        .bind()
        .await
        .context("binding controller endpoint")?;
    // online() waits for a home-relay connection — which never comes when
    // relays are disabled, and would hang the controller before it serves.
    if !matches!(mode, RelayMode::Disabled) {
        endpoint.online().await;
    }

    let agents: Agents = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let handler = AgentHandler {
        state_dir: Arc::new(opts.state_dir.clone()),
        agents: agents.clone(),
    };
    let router = Router::builder(endpoint.clone())
        .accept(ALPN, handler)
        .accept(CANARY_ALPN, CanaryHandler)
        .spawn();
    Ok(Running {
        endpoint,
        agents,
        state_dir: opts.state_dir,
        relay_mode: mode,
        _router: router,
    })
}

/// The control-socket accept loop: bridges local CLI sessions onto agent
/// connections. Runs forever; spawn it and abort to stop.
pub async fn bridge_listener(state_dir: PathBuf, agents: Agents) -> Result<()> {
    let sock_path = state_dir.join("control.sock");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding {}", sock_path.display()))?;
    println!("egdod controller control socket: {}", sock_path.display());
    loop {
        let (sock, _) = listener.accept().await?;
        let agents = agents.clone();
        tokio::spawn(async move {
            if let Err(e) = bridge_session(sock, agents).await {
                tracing::debug!("control session ended: {e:#}");
            }
        });
    }
}

/// The control socket is a dumb bridge: after "connect <pubkey>" it splices
/// the local stream onto a fresh stream to that agent. Protocol knowledge
/// stays in the CLI and the agent; serve only routes.
async fn bridge_session(
    sock: UnixStream,
    agents: Agents,
) -> Result<()> {
    let (read, mut write) = sock.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let pk = match line.strip_prefix("connect ") {
        Some(p) => p.trim().parse::<PublicKey>(),
        None => {
            write.write_all(b"err expected: connect <pubkey>\n").await?;
            return Ok(());
        }
    };
    let conn = match pk {
        Ok(pk) => agents.lock().await.get(&pk).map(|(_, c)| c.clone()),
        Err(_) => None,
    };
    let conn = match conn {
        Some(c) if c.close_reason().is_none() => c,
        _ => {
            write.write_all(b"err agent not connected\n").await?;
            return Ok(());
        }
    };
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(s) => s,
        Err(e) => {
            write
                .write_all(format!("err opening stream to agent: {e:#}\n").as_bytes())
                .await?;
            return Ok(());
        }
    };
    write.write_all(b"ok\n").await?;
    // BufReader buffered nothing beyond the line (the client waits for "ok"
    // before sending protocol bytes), so into_inner loses no data.
    let read = reader.into_inner();
    let sock = read.reunite(write)?;
    let (mut sr, mut sw) = sock.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut sr, &mut send).await;
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut sw).await;
        let _ = sw.shutdown().await;
    };
    tokio::join!(up, down);
    Ok(())
}

// ---------------------------------------------------------------------------
// Approval state on disk: approved/<pubkey>, pending/<pubkey>. Plain files so
// approval survives restart and is scriptable with mv/echo as well as the CLI.
// ---------------------------------------------------------------------------

pub fn is_approved(state_dir: &Path, pk: &PublicKey) -> bool {
    state_dir.join("approved").join(pk.to_string()).exists()
}

pub fn approve(state_dir: &Path, pk: &PublicKey) -> Result<bool> {
    std::fs::create_dir_all(state_dir.join("approved"))?;
    let pending = state_dir.join("pending").join(pk.to_string());
    let was_pending = pending.exists();
    let approved = state_dir.join("approved").join(pk.to_string());
    let now = unix_now();
    std::fs::write(&approved, format!("approved_at={now}\n"))?;
    if was_pending {
        let _ = std::fs::remove_file(pending);
    }
    Ok(was_pending)
}

fn record_pending(state_dir: &Path, pk: &PublicKey) -> Result<()> {
    std::fs::create_dir_all(state_dir.join("pending"))?;
    let path = state_dir.join("pending").join(pk.to_string());
    let now = unix_now();
    // first_seen survives re-knocks; last_seen is refreshed.
    let first = std::fs::read_to_string(&path)
        .ok()
        .and_then(|body| {
            body.lines()
                .find_map(|l| l.strip_prefix("first_seen=").map(|s| s.to_string()))
        })
        .unwrap_or_else(|| now.to_string());
    std::fs::write(&path, format!("first_seen={first}\nlast_seen={now}\n"))?;
    Ok(())
}

pub struct PendingEntry {
    pub pubkey: String,
    pub first_seen: u64,
    pub last_seen: u64,
}

pub fn list_pending(state_dir: &Path) -> Result<Vec<PendingEntry>> {
    let mut out = Vec::new();
    let dir = state_dir.join("pending");
    if !dir.exists() {
        return Ok(out);
    }
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let pubkey = e.file_name().to_string_lossy().into_owned();
        // Files not named like a key are foreign; leave them alone.
        if pubkey.parse::<PublicKey>().is_err() {
            continue;
        }
        let body = std::fs::read_to_string(e.path()).unwrap_or_default();
        let mut first_seen = 0;
        let mut last_seen = 0;
        for l in body.lines() {
            if let Some(v) = l.strip_prefix("first_seen=") {
                first_seen = v.parse().unwrap_or(0);
            }
            if let Some(v) = l.strip_prefix("last_seen=") {
                last_seen = v.parse().unwrap_or(0);
            }
        }
        out.push(PendingEntry {
            pubkey,
            first_seen,
            last_seen,
        });
    }
    out.sort_by(|a, b| a.first_seen.cmp(&b.first_seen));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Reachability canary (spec: the controller must know when it is undialable).
// A separate in-process endpoint dials our own NodeId with no address hints,
// so it exercises exactly what a booting target exercises: discovery + relay.
// ---------------------------------------------------------------------------

async fn canary_loop(mode: RelayMode, state_dir: PathBuf, id: EndpointId) {
    // First check soon after boot — the publish race the prior art hit shows
    // up in the first minutes, not after a steady state.
    let mut first = true;
    loop {
        if !first {
            tokio::time::sleep(CANARY_INTERVAL).await;
        }
        first = false;
        let outcome = canary_once(&mode, id).await;
        let rec = match &outcome {
            Ok(path) => {
                serde_json::json!({"ok": true, "checked_at": unix_now(), "path": path})
            }
            Err(e) => {
                println!(
                    "egdod serve: WARNING controller appears UNDIALABLE by NodeId {id}: {e:#} \
                     (targets booting now cannot find me; check discovery publish / relay)"
                );
                serde_json::json!({"ok": false, "checked_at": unix_now(), "error": format!("{e:#}")})
            }
        };
        if let Err(e) = std::fs::write(
            state_dir.join("reachability.json"),
            serde_json::to_string_pretty(&rec).unwrap(),
        ) {
            tracing::warn!("writing reachability.json: {e}");
        }
    }
}

async fn canary_once(mode: &RelayMode, id: EndpointId) -> Result<String> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    let ep = Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&seed))
        .relay_mode(mode.clone())
        .bind()
        .await
        .context("binding canary endpoint")?;
    let dial = ep.connect(EndpointAddr::new(id), CANARY_ALPN);
    let conn = match tokio::time::timeout(CANARY_TIMEOUT, dial).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            ep.close().await;
            return Err(e).context("self-dial by NodeId failed");
        }
        Err(_) => {
            ep.close().await;
            anyhow::bail!("self-dial by NodeId timed out after {CANARY_TIMEOUT:?}");
        }
    };
    let path = classify_path(&conn);
    conn.close(0u32.into(), b"canary done");
    ep.close().await;
    Ok(path)
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
