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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
    /// Prefixed to the installed line. Nothing ever removes that line, so the
    /// default pins it to the target's own loopback, where the forward exits:
    /// useless to anyone not already on the target.
    pub key_options: String,
    /// `expiry-time=` on the same line, judged by the target's clock.
    pub key_ttl: Duration,
}

pub async fn run(state: &StateDir, cfg: Config) -> Result<i32> {
    let (key, pubkey, known_hosts) = (
        state.ssh_key_path(),
        state.ssh_pub_path(),
        state.known_hosts_path(),
    );
    std::fs::create_dir_all(state.ssh_dir())?;
    ensure_local_key(&key)?;
    let pubkey_line = std::fs::read_to_string(&pubkey)
        .with_context(|| format!("reading {}", pubkey.display()))?
        .trim()
        .to_string();
    // Per-process, so two concurrent `ssh` runs against one state dir cannot
    // stage over each other.
    let scratch = state.ssh_dir().join(format!("scratch.{}", std::process::id()));

    let blob = key_blob(&pubkey_line)
        .with_context(|| format!("{} is not an ssh public key", pubkey.display()))?;
    let line = authorized_line(&cfg.key_options, SystemTime::now() + cfg.key_ttl, &pubkey_line);
    install_authorized_key(state, &cfg, blob, &line, &scratch).await?;
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
    // so it is rewritten rather than appended to. ssh only looks up the bracketed
    // form for a non-default port, so port 22 must be written bare or the lookup
    // misses and StrictHostKeyChecking refuses the connection.
    let host = if bound.port() == 22 {
        bound.ip().to_string()
    } else {
        format!("[{}]:{}", bound.ip(), bound.port())
    };
    let entry = format!("{host} {host_key}\n");
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
    let _ = std::fs::remove_file(&scratch);
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

fn key_blob(pubkey_line: &str) -> Option<&str> {
    pubkey_line.split_whitespace().nth(1)
}

fn authorized_line(options: &str, expiry: SystemTime, pubkey_line: &str) -> String {
    format!("{options},expiry-time={} {pubkey_line}", utc_yyyymmddhhmm(expiry))
}

/// The `Z` form: without it sshd judges the stamp in the target's local zone,
/// which an initramfs does not have.
fn utc_yyyymmddhhmm(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hh, mm) = (rem / 3600, rem % 3600 / 60);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}{m:02}{d:02}{hh:02}{mm:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Every line carrying this key blob is dropped before the new one goes in, so
/// a rerun renews the expiry instead of stacking a line per run.
fn merge_authorized_keys(existing: &str, blob: &str, line: &str) -> String {
    let mut body: String = existing
        .lines()
        .filter(|l| !l.split_whitespace().any(|f| f == blob))
        .map(|l| format!("{l}\n"))
        .collect();
    body.push_str(line);
    body.push('\n');
    body
}

/// pull + edit + push: whatever else the target's authorized_keys holds is
/// preserved.
async fn install_authorized_key(
    state: &StateDir,
    cfg: &Config,
    blob: &str,
    line: &str,
    scratch: &Path,
) -> Result<()> {
    let existing = controller::pull_bytes(state, &cfg.agent, &cfg.authorized_keys, scratch).await?;
    let existing = existing
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let body = merge_authorized_keys(&existing, blob, line);
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
    eprintln!("egdod: installed at {}: {line}", cfg.authorized_keys);
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

#[cfg(test)]
mod tests {
    use super::*;

    const PK: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGxx egdod-controller";

    #[test]
    fn line_carries_options_and_utc_expiry() {
        let t = UNIX_EPOCH + Duration::from_secs(1_788_264_600);
        assert_eq!(
            authorized_line(r#"from="127.0.0.1,::1""#, t, PK),
            format!(r#"from="127.0.0.1,::1",expiry-time=202609011210Z {PK}"#)
        );
    }

    #[test]
    fn civil_dates_match_the_calendar() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_089), (2025, 1, 1));
        let last_minute = UNIX_EPOCH + Duration::from_secs(951_782_400 + 86_399);
        assert_eq!(utc_yyyymmddhhmm(last_minute), "200002292359Z");
    }

    #[test]
    fn rerun_replaces_by_blob_and_keeps_strangers() {
        let blob = key_blob(PK).unwrap();
        let stale = format!("expiry-time=202601010000Z {PK}");
        let other = "ssh-rsa AAAAB3other someone-else";
        let fresh = format!("expiry-time=202701010000Z {PK}");
        let once = merge_authorized_keys(&format!("{other}\n{stale}"), blob, &fresh);
        assert_eq!(once, format!("{other}\n{fresh}\n"));
        assert_eq!(merge_authorized_keys(&once, blob, &fresh), once);
        assert_eq!(merge_authorized_keys("", blob, &fresh), format!("{fresh}\n"));
        assert_eq!(merge_authorized_keys(other, blob, &fresh), format!("{other}\n{fresh}\n"));
    }
}
