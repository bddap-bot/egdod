//! The wire format, shared by every egdod link.
//!
//! One frame codec serves three different byte streams — a QUIC bi stream to an
//! agent, the controller's local unix socket, and an in-memory duplex in the
//! tests — so everything here is generic over `AsyncRead`/`AsyncWrite` instead of
//! naming iroh types. That is also what lets `serve` be a dumb proxy: it reads
//! one routing message off the unix socket and then copies bytes verbatim, so
//! the request protocol has exactly one implementation on each side.

use anyhow::{bail, ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Agent connections. Bumping this is how an incompatible protocol change is made.
pub const ALPN: &[u8] = b"egdod/0";
/// Reachability canary: the controller answers this from any dialer, because the
/// point is to prove a *stranger* can find and reach it.
pub const PROBE_ALPN: &[u8] = b"egdod-probe/0";
pub const PROBE_PING: &[u8] = b"egdod-ping";
pub const PROBE_PONG: &[u8] = b"egdod-alive";

/// QUIC application close code the controller uses to turn away an agent whose
/// key has not been approved. The agent keys its retry policy off this, which is
/// the only way a screenless target can log "I am waiting to be approved".
pub const CLOSE_PENDING: u32 = 1;

/// Bound on a single control message. Bulk data never goes through a message, so
/// this only has to fit an argv or a path.
const MAX_MSG: usize = 1 << 20;
const CHUNK: usize = 64 * 1024;

/// One request per stream. The stream *is* the request's channel: what follows
/// the request message depends on the variant, and end-of-request is stream FIN.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Then: `ExecFrame`s from the agent until FIN.
    Exec { argv: Vec<String> },
    /// Then: `len` raw bytes from the controller, FIN, and one `Ack` back.
    Push {
        path: String,
        mode: u32,
        len: u64,
        sha256: [u8; 32],
    },
    /// Then: one `PullStart` from the agent, followed by the bytes and FIN.
    Pull { path: String },
    /// Then: one `Ack`, after which the stream is a raw pipe to `addr`.
    Forward { addr: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ExecFrame {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    /// Signal deaths are reported as 128+signal, the shell convention, because
    /// the wire type has to be a single number the controller can exit with.
    Exit(i32),
    /// The command could not be run at all (as distinct from running and failing).
    Failed(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Ack {
    Ok,
    Err(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum PullStart {
    Ok { mode: u32, len: u64, sha256: [u8; 32] },
    Err(String),
}

/// First message on the controller's local unix socket: which agent this stream
/// is for. Everything after it is the agent protocol above, proxied verbatim.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Full agent public key, a unique prefix of one, or "any" when exactly one
    /// agent is connected.
    pub agent: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RouteReply {
    Ok {
        agent: String,
        path: PathKind,
        remote: String,
    },
    Err(String),
}

/// How the packets for a session are actually travelling. Reported for every
/// session because a relayed connection that looks direct is a security-relevant
/// lie, not a cosmetic one.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PathKind {
    /// Same host (loopback).
    DirectLocal,
    /// Direct UDP to a private/link-local address: same LAN, no relay.
    DirectLan,
    /// Direct UDP to a globally routable address: no relay, but not local either.
    /// The spec names three paths; this is split out of "direct-LAN" rather than
    /// mislabelling an internet-routed direct path as LAN.
    DirectWan,
    /// Through a relay server.
    Relayed,
    /// A path iroh reported that this code cannot positively classify. It exists
    /// so that an unrecognised path is never rounded up to "direct".
    Unknown,
}

impl std::fmt::Display for PathKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PathKind::DirectLocal => "direct-local",
            PathKind::DirectLan => "direct-lan",
            PathKind::DirectWan => "direct-wan",
            PathKind::Relayed => "relayed",
            PathKind::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = postcard::to_allocvec(msg).context("encoding message")?;
    ensure!(body.len() <= MAX_MSG, "message too large: {}", body.len());
    let len = u32::try_from(body.len()).expect("checked against MAX_MSG above");
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_msg<R, T>(r: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    read_msg_opt(r)
        .await?
        .context("peer closed the stream before sending a message")
}

/// `None` on a clean end of stream, which is how framed sequences (exec output)
/// terminate.
pub async fn read_msg_opt<R, T>(r: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("reading message length"),
    }
    let len = u32::from_le_bytes(len) as usize;
    ensure!(len <= MAX_MSG, "peer announced an oversized message: {len}");
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.context("reading message")?;
    Ok(Some(postcard::from_bytes(&body).context("decoding message")?))
}

/// Digest, length and mode of a file, read in fixed-size chunks so a file larger
/// than RAM costs no more memory than a small one.
///
/// The integrity header has to precede the payload (a trailer would need a second
/// framing layer around the bulk stream), which is why the sender reads the file
/// twice: once here, once to send it.
pub async fn stat_and_hash(path: &Path) -> Result<(u32, u64, [u8; 32])> {
    let mut f = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let meta = f.metadata().await?;
    ensure!(meta.is_file(), "{} is not a regular file", path.display());
    let mode = mode_of(&meta);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut len = 0u64;
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((mode, len, hasher.finalize().into()))
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

pub async fn send_file_body<W>(w: &mut W, path: &Path) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut f = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).await?;
    }
    w.flush().await?;
    Ok(())
}

/// Streams the rest of `r` into `dest`, verifying length and digest before the
/// destination path is allowed to exist: a truncated or corrupted transfer
/// leaves the target untouched rather than half-written.
pub async fn recv_file_body<R>(
    r: &mut R,
    dest: &Path,
    len: u64,
    sha256: [u8; 32],
    mode: u32,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    // The agent may be writing into a directory that does not exist yet and has
    // no shell available to create one.
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let tmp = temp_path(dest);
    let mut out = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut got = 0u64;
    let outcome = loop {
        let n = match r.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => break Err(anyhow::Error::from(e).context("reading file body")),
        };
        if n == 0 {
            break Ok(());
        }
        hasher.update(&buf[..n]);
        got += n as u64;
        if let Err(e) = out.write_all(&buf[..n]).await {
            break Err(anyhow::Error::from(e).context("writing file body"));
        }
    };
    let flushed = out.flush().await;
    drop(out);
    let verdict = (|| {
        outcome?;
        flushed?;
        ensure!(got == len, "expected {len} bytes, got {got}");
        let digest: [u8; 32] = hasher.finalize().into();
        ensure!(digest == sha256, "sha256 mismatch");
        Ok(())
    })();
    if verdict.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return verdict;
    }
    set_mode(&tmp, mode).await?;
    tokio::fs::rename(&tmp, dest)
        .await
        .with_context(|| format!("renaming into {}", dest.display()))?;
    Ok(())
}

fn temp_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".egdod-part");
    dest.with_file_name(name)
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .with_context(|| format!("setting mode on {}", path.display()))
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Half-closes the write side of a duplex stream the way each transport spells it.
pub async fn finish<W: AsyncWrite + Unpin>(w: &mut W) -> Result<()> {
    w.flush().await?;
    w.shutdown().await?;
    Ok(())
}

pub fn parse_pubkey(s: &str) -> Result<iroh::PublicKey> {
    match s.parse::<iroh::PublicKey>() {
        Ok(k) => Ok(k),
        Err(e) => bail!("not a valid public key ({e}): {s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn msg_roundtrip_and_framing() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let req = Request::Exec {
            argv: vec!["/bin/true".into(), "arg".into()],
        };
        let sent = req.clone();
        let w = tokio::spawn(async move {
            write_msg(&mut a, &sent).await.unwrap();
            write_msg(&mut a, &Ack::Err("nope".into())).await.unwrap();
            finish(&mut a).await.unwrap();
        });
        let got: Request = read_msg(&mut b).await.unwrap();
        assert_eq!(got, req);
        let ack: Ack = read_msg(&mut b).await.unwrap();
        assert_eq!(ack, Ack::Err("nope".into()));
        // Clean EOF is how a framed sequence ends, and must not be an error.
        assert_eq!(read_msg_opt::<_, Ack>(&mut b).await.unwrap(), None);
        w.await.unwrap();
    }

    #[tokio::test]
    async fn copy_verifies_digest_and_preserves_mode() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        // Bigger than one chunk, to exercise the streaming loop.
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&src, &body).await.unwrap();
        set_mode(&src, 0o750).await.unwrap();

        let (mode, len, sha) = stat_and_hash(&src).await.unwrap();
        assert_eq!(mode, 0o750);
        assert_eq!(len, body.len() as u64);

        let (mut w, mut r) = tokio::io::duplex(4096);
        let s = src.clone();
        let sender = tokio::spawn(async move {
            send_file_body(&mut w, &s).await.unwrap();
            finish(&mut w).await.unwrap();
        });
        let dest = dir.path().join("nested/dest.bin");
        recv_file_body(&mut r, &dest, len, sha, mode).await.unwrap();
        sender.await.unwrap();
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), body);
        assert_eq!(stat_and_hash(&dest).await.unwrap(), (mode, len, sha));
    }

    #[tokio::test]
    async fn corrupt_transfer_leaves_no_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest.bin");
        let (mut w, mut r) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            w.write_all(b"tampered").await.unwrap();
            finish(&mut w).await.unwrap();
        });
        let err = recv_file_body(&mut r, &dest, 8, [0u8; 32], 0o644)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
        assert!(!dest.exists());
        assert!(!temp_path(&dest).exists());
    }

    #[tokio::test]
    async fn truncated_transfer_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest.bin");
        let (mut w, mut r) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            w.write_all(b"abc").await.unwrap();
            finish(&mut w).await.unwrap();
        });
        let (_, _, sha) = {
            let mut h = Sha256::new();
            h.update(b"abcdef");
            (0u32, 6u64, <[u8; 32]>::from(h.finalize()))
        };
        let err = recv_file_body(&mut r, &dest, 6, sha, 0o644)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expected 6 bytes"), "{err}");
        assert!(!dest.exists());
    }
}
