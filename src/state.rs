//! Everything the controller remembers, under one directory.
//!
//! One directory and no fixed system paths is what makes relocating a controller
//! `cp -a`, including onto a handset — so nothing here reaches outside `root`,
//! and nothing needs root to write.

use crate::net::ProbeMode;
use crate::proto::PathKind;
use anyhow::{bail, Context, Result};
use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct StateDir {
    root: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PendingAgent {
    pub pubkey: String,
    pub first_seen_unix: u64,
    pub last_seen_unix: u64,
    pub attempts: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionInfo {
    pub agent: String,
    pub path: PathKind,
    pub remote: String,
    pub since_unix: u64,
}

/// What `serve` publishes about itself, so another program (or a human) can ask
/// whether the controller is merely up or actually findable.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Status {
    pub node_id: String,
    pub pid: u32,
    pub updated_unix: u64,
    pub direct_addrs: Vec<String>,
    /// `None` until the first probe completes.
    pub dialable: Option<bool>,
    pub probe_mode: ProbeMode,
    pub last_probe_ok_unix: Option<u64>,
    pub last_probe_path: Option<PathKind>,
    pub last_probe_error: Option<String>,
    pub consecutive_probe_failures: u32,
    pub endpoint_restarts: u32,
    pub sessions: Vec<SessionInfo>,
}

/// Most-recent pending agents kept. See `record_pending`.
const PENDING_LIMIT: usize = 256;

impl Status {
    /// Whether the process that wrote this file is still alive. Without it,
    /// `status` would cheerfully report a dead controller's last opinion of
    /// itself — the exact "looks healthy" failure the spec is about.
    pub fn writer_alive(&self) -> bool {
        Path::new(&format!("/proc/{}", self.pid)).exists()
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl StateDir {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn default_path() -> PathBuf {
        if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(x).join("egdod");
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join(".local/state/egdod")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn key_path(&self) -> PathBuf {
        self.root.join("controller.key")
    }
    pub fn sock_path(&self) -> PathBuf {
        self.root.join("control.sock")
    }
    pub fn approved_path(&self) -> PathBuf {
        self.root.join("approved")
    }
    pub fn pending_path(&self) -> PathBuf {
        self.root.join("pending.json")
    }
    pub fn status_path(&self) -> PathBuf {
        self.root.join("status.json")
    }
    pub fn ssh_dir(&self) -> PathBuf {
        self.root.join("ssh")
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating state dir {}", self.root.display()))?;
        restrict(&self.root)?;
        Ok(())
    }

    /// The NodeId derived from this key is the one public thing that gets baked
    /// into images, so creating it is `init`'s whole job.
    pub fn init_key(&self) -> Result<SecretKey> {
        self.ensure()?;
        load_or_create_key(&self.key_path(), OnUnusable::Refuse)
    }

    pub fn load_key(&self) -> Result<SecretKey> {
        match read_key(&self.key_path())? {
            Some(k) => Ok(k),
            None => bail!(
                "no controller key at {} — run `egdod controller init` first",
                self.key_path().display()
            ),
        }
    }

    pub fn ssh_key_path(&self) -> PathBuf {
        self.ssh_dir().join("id_ed25519")
    }
    pub fn ssh_pub_path(&self) -> PathBuf {
        self.ssh_dir().join("id_ed25519.pub")
    }
    pub fn known_hosts_path(&self) -> PathBuf {
        self.ssh_dir().join("known_hosts")
    }

    /// Read fresh on every connection attempt, so `approve` takes effect without
    /// restarting `serve`, and an approval made while `serve` was down still holds.
    pub fn approved(&self) -> Result<BTreeSet<PublicKey>> {
        let body = match std::fs::read_to_string(self.approved_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(e) => return Err(e).context("reading approved list"),
        };
        let mut out = BTreeSet::new();
        for (n, line) in body.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let key = line
                .split_whitespace()
                .next()
                .expect("non-empty line has a first field");
            match key.parse::<PublicKey>() {
                Ok(k) => {
                    out.insert(k);
                }
                Err(e) => tracing::warn!("approved:{}: ignoring unparseable key ({e})", n + 1),
            }
        }
        Ok(out)
    }

    /// Returns false if the key was already approved.
    pub fn approve(&self, key: &PublicKey) -> Result<bool> {
        self.ensure()?;
        if self.approved()?.contains(key) {
            return Ok(false);
        }
        let mut body = std::fs::read_to_string(self.approved_path()).unwrap_or_default();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&format!("{key}\n"));
        write_atomic(&self.approved_path(), body.as_bytes())?;
        self.forget_pending(key)?;
        Ok(true)
    }

    pub fn pending(&self) -> Result<Vec<PendingAgent>> {
        match std::fs::read(self.pending_path()) {
            Ok(b) => Ok(serde_json::from_slice(&b).context("parsing pending.json")?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).context("reading pending.json"),
        }
    }

    pub fn record_pending(&self, key: &PublicKey) -> Result<()> {
        self.ensure()?;
        let now = now_unix();
        let mut list = self.pending()?;
        let text = key.to_string();
        match list.iter_mut().find(|p| p.pubkey == text) {
            Some(p) => {
                p.last_seen_unix = now;
                p.attempts += 1;
            }
            None => list.push(PendingAgent {
                pubkey: text,
                first_seen_unix: now,
                last_seen_unix: now,
                attempts: 1,
            }),
        }
        // The node id is public, so anyone can dial with a fresh key and add an
        // entry. Keeping only the most recent PENDING_LIMIT bounds what a stranger
        // can cost a controller that may be a phone.
        if list.len() > PENDING_LIMIT {
            list.sort_by_key(|p| p.last_seen_unix);
            let excess = list.len() - PENDING_LIMIT;
            list.drain(..excess);
        }
        self.write_pending(&list)
    }

    pub fn forget_pending(&self, key: &PublicKey) -> Result<()> {
        let text = key.to_string();
        let mut list = self.pending()?;
        let before = list.len();
        list.retain(|p| p.pubkey != text);
        if list.len() != before {
            self.write_pending(&list)?;
        }
        Ok(())
    }

    fn write_pending(&self, list: &[PendingAgent]) -> Result<()> {
        let body = serde_json::to_vec_pretty(list)?;
        write_atomic(&self.pending_path(), &body)
    }

    pub fn write_status(&self, status: &Status) -> Result<()> {
        let body = serde_json::to_vec_pretty(status)?;
        write_atomic(&self.status_path(), &body)
    }

    pub fn read_status(&self) -> Result<Status> {
        let body = std::fs::read(self.status_path()).with_context(|| {
            format!(
                "reading {} — is `egdod controller serve` running?",
                self.status_path().display()
            )
        })?;
        serde_json::from_slice(&body).context("parsing status.json")
    }
}

fn read_key(path: &Path) -> Result<Option<SecretKey>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                anyhow::anyhow!("key file {} is not 32 bytes", path.display())
            })?;
            warn_if_readable(path);
            Ok(Some(SecretKey::from_bytes(&arr)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// What to do about a key file that exists but cannot be used.
///
/// The two roles need opposite answers. A target has no screen and no operator,
/// so a key truncated by a power cut during first boot must be replaced or the
/// machine is bricked. A controller has an operator, and its key *is* the
/// identity baked into every image — silently minting a new one would strand
/// every target that already carries the old NodeId.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OnUnusable {
    Replace,
    Refuse,
}

/// Loads the key at `path`, creating one if there is none. Shared by both roles
/// so there is one answer to "where does an identity come from".
pub fn load_or_create_key(path: &Path, unusable: OnUnusable) -> Result<SecretKey> {
    match read_key(path) {
        Ok(Some(k)) => return Ok(k),
        Ok(None) => {}
        Err(e) if unusable == OnUnusable::Replace => {
            tracing::error!("replacing unusable key file {}: {e:#}", path.display());
        }
        Err(e) => return Err(e),
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            // Only a directory this call created is restricted: the key file may
            // legitimately live in a shared directory, and an agent running as
            // init that chmod-0700'd, say, /etc would take the machine down.
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
            restrict(parent)?;
        }
    }
    write_private(path, &bytes)?;
    Ok(SecretKey::from_bytes(&bytes))
}

fn warn_if_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            tracing::warn!(
                "{} is mode {:o}: the key that owns every approved target is readable by others",
                path.display(),
                mode & 0o7777
            );
        }
    }
}

fn restrict(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting {}", dir.display()))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // Written to a side file and renamed into place, fsynced first: an identity
    // that exists must be complete, because half a key file is indistinguishable
    // from a whole one by length alone on the next boot.
    let tmp = unique_tmp(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(bytes).context("writing key")?;
    f.sync_all().context("flushing key")?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

/// A staging path unique to this process, because `serve` and a concurrent
/// `approve` write the same files and a shared temp name would let them splice
/// each other's bytes into one unparseable file.
fn unique_tmp(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

/// Temp-then-rename: a reader (`pending --json` on a script's timer, or a restart
/// mid-write) must never see a half-written file.
fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let tmp = unique_tmp(path);
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> PublicKey {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    #[test]
    fn approval_is_by_pubkey_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path().join("egdod"));
        let a = key(1);
        let b = key(2);

        state.record_pending(&a).unwrap();
        state.record_pending(&a).unwrap();
        state.record_pending(&b).unwrap();
        let pending = state.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].attempts, 2);

        assert!(state.approved().unwrap().is_empty());
        assert!(state.approve(&a).unwrap());
        assert!(!state.approve(&a).unwrap(), "second approve is a no-op");

        // A fresh handle is what a restarted controller sees.
        let reopened = StateDir::new(dir.path().join("egdod"));
        assert!(reopened.approved().unwrap().contains(&a));
        assert!(!reopened.approved().unwrap().contains(&b));
        let pending = reopened.pending().unwrap();
        assert_eq!(pending.len(), 1, "approved agent leaves the pending list");
        assert_eq!(pending[0].pubkey, b.to_string());
    }

    #[test]
    fn approved_list_tolerates_comments_and_junk() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path().to_path_buf());
        state.ensure().unwrap();
        std::fs::write(
            state.approved_path(),
            format!("# a comment\n\n{}  the lab machine\nnot-a-key\n", key(3)),
        )
        .unwrap();
        let approved = state.approved().unwrap();
        assert_eq!(approved.len(), 1);
        assert!(approved.contains(&key(3)));
    }

    #[test]
    fn key_is_stable_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path().join("s"));
        let first = state.init_key().unwrap();
        let second = state.init_key().unwrap();
        assert_eq!(first.public(), second.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(state.key_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "key must not be group/world readable");
        }
    }
}
