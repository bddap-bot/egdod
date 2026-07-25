//! `ssh`, assembled out of exec + copy + forward. No new transport, no new
//! protocol message: every step below is one of the three primitives.
//!
//! The point of the recipe is the first connect. Because the host key travels
//! back over the egdod channel — which QUIC already authenticated against the
//! agent key the operator approved — ssh can be run with
//! `StrictHostKeyChecking=yes -o BatchMode=yes`: no prompt, no trust-on-first-use.

use crate::controller;
use crate::state::StateDir;
use anyhow::{bail, Context, Result};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub struct Config {
    pub agent: String,
    pub user: String,
    pub authorized_keys: String,
    pub host_key_pub: String,
    /// argv that starts an sshd on the target, run only if nothing answers on
    /// `target_addr`. Repeatable on the command line because the agent has no
    /// shell to split a string for it.
    pub sshd_argv: Vec<String>,
    /// argv that generates the target's host keys, run only if the host key is
    /// missing.
    pub keygen_argv: Vec<String>,
    pub target_addr: String,
    pub local_port: u16,
    pub ssh_argv: Vec<String>,
}

pub async fn run(state: &StateDir, cfg: Config) -> Result<i32> {
    let (key, pubkey, known_hosts) = controller::ssh_paths(state);
    std::fs::create_dir_all(state.ssh_dir())?;
    ensure_local_key(&key)?;
    let pubkey_line = std::fs::read_to_string(&pubkey)
        .with_context(|| format!("reading {}", pubkey.display()))?
        .trim()
        .to_string();
    let scratch = state.ssh_dir().join("scratch");

    install_authorized_key(state, &cfg, &pubkey_line, &scratch).await?;
    let host_key = fetch_host_key(state, &cfg, &scratch).await?;

    let local: SocketAddr = ([127, 0, 0, 1], cfg.local_port).into();
    let (bound, forwarder) = controller::forward_listener(
        state,
        &cfg.agent,
        local,
        cfg.target_addr.clone(),
    )
    .await?;

    ensure_sshd(state, &cfg, bound).await?;

    // known_hosts is keyed by the forwarded port, which changes per invocation,
    // so it is rewritten rather than appended to.
    let entry = format!("[{}]:{} {}\n", bound.ip(), bound.port(), host_key);
    std::fs::write(&known_hosts, &entry)
        .with_context(|| format!("writing {}", known_hosts.display()))?;
    eprintln!("egdod: known_hosts entry from the authenticated channel: {}", entry.trim());

    let status = tokio::process::Command::new("ssh")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        .arg("-i")
        .arg(&key)
        .arg("-p")
        .arg(bound.port().to_string())
        .arg(format!("{}@{}", cfg.user, bound.ip()))
        .args(&cfg.ssh_argv)
        .status()
        .await
        .context("running ssh (is an ssh client installed on the controller?)")?;
    forwarder.abort();
    Ok(status.code().unwrap_or(255))
}

fn ensure_local_key(key: &Path) -> Result<()> {
    if key.exists() {
        return Ok(());
    }
    let status = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "egdod-controller", "-f"])
        .arg(key)
        .status()
        .context("running ssh-keygen on the controller")?;
    if !status.success() {
        bail!("ssh-keygen failed: {status}");
    }
    Ok(())
}

/// pull + edit + push: the authorized_keys file the target already has is
/// preserved, which matters when the recipe is run twice or alongside anything
/// else that put a key there.
async fn install_authorized_key(
    state: &StateDir,
    cfg: &Config,
    line: &str,
    scratch: &Path,
) -> Result<()> {
    let existing = controller::pull_bytes(state, &cfg.agent, &cfg.authorized_keys, scratch).await?;
    let mut body = existing
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    if body.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(line);
    body.push('\n');
    controller::push_bytes(
        state,
        &cfg.agent,
        &cfg.authorized_keys,
        body.as_bytes(),
        0o600,
        scratch,
    )
    .await
    .context("installing the authorized key")?;
    eprintln!("egdod: installed an authorized key at {}", cfg.authorized_keys);
    Ok(())
}

async fn fetch_host_key(state: &StateDir, cfg: &Config, scratch: &Path) -> Result<String> {
    if let Some(k) = read_host_key(state, cfg, scratch).await? {
        return Ok(k);
    }
    eprintln!("egdod: no host key on the target; generating one with {:?}", cfg.keygen_argv);
    let (code, _out, err) =
        controller::exec_capture(state, &cfg.agent, cfg.keygen_argv.clone()).await?;
    if code != 0 {
        bail!(
            "host key generation failed on the target (exit {code}): {}",
            String::from_utf8_lossy(&err).trim()
        );
    }
    read_host_key(state, cfg, scratch)
        .await?
        .with_context(|| format!("{} still absent after key generation", cfg.host_key_pub))
}

async fn read_host_key(state: &StateDir, cfg: &Config, scratch: &Path) -> Result<Option<String>> {
    let Some(raw) = controller::pull_bytes(state, &cfg.agent, &cfg.host_key_pub, scratch).await?
    else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&raw);
    let mut fields = text.split_whitespace();
    match (fields.next(), fields.next()) {
        // The trailing comment is dropped: known_hosts only needs type and key.
        (Some(kind), Some(blob)) => Ok(Some(format!("{kind} {blob}"))),
        _ => bail!("{} is not an ssh public key", cfg.host_key_pub),
    }
}

/// Starts sshd only if nothing is already answering, and proves the outcome by
/// reading the SSH banner through the forward — a TCP connect to our own
/// forwarder would succeed even with nothing listening on the target.
async fn ensure_sshd(state: &StateDir, cfg: &Config, bound: SocketAddr) -> Result<()> {
    if banner(bound).await.is_some() {
        return Ok(());
    }
    eprintln!("egdod: no sshd answering; starting it with {:?}", cfg.sshd_argv);
    let (code, _out, err) =
        controller::exec_capture(state, &cfg.agent, cfg.sshd_argv.clone()).await?;
    if code != 0 {
        bail!(
            "starting sshd failed (exit {code}): {}",
            String::from_utf8_lossy(&err).trim()
        );
    }
    for _ in 0..20 {
        if let Some(b) = banner(bound).await {
            eprintln!("egdod: sshd is up: {b}");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    bail!("sshd was started but never answered on {}", cfg.target_addr)
}

async fn banner(addr: SocketAddr) -> Option<String> {
    let mut stream = tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&buf[..n]).trim().to_string();
    text.starts_with("SSH-").then_some(text)
}
