//! egdod shared library: keys, the wire protocol for the three primitives, and
//! per-session path reporting. Both roles (controller, agent) link this; the
//! protocol lives here exactly once so the two sides cannot drift.

pub mod agent;
pub mod cmd;
pub mod serve;

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::{SecretKey, TransportAddr};
use sha2::Digest;
use std::net::IpAddr;
use std::path::Path;

/// Controller<->agent ALPN. The agent dials it; after approval every stream on
/// the connection is controller-opened and carries one primitive invocation.
pub const ALPN: &[u8] = b"egdod/0";

/// Separate ALPN for the controller's self-dial canary, so a canary connection
/// is never mistaken for an unapproved agent (which would poison `pending`).
pub const CANARY_ALPN: &[u8] = b"egdod-canary/0";

pub mod op {
    pub const EXEC: u8 = 1;
    pub const PUT: u8 = 2;
    pub const GET: u8 = 3;
    pub const FWD: u8 = 4;
}

pub mod frame {
    pub const STDOUT: u8 = 1;
    pub const STDERR: u8 = 2;
    pub const EXIT: u8 = 3;
}

pub const STATUS_OK: u8 = 0;
pub const STATUS_ERR: u8 = 1;

/// Frames and headers are length-prefixed with u32; cap them so a corrupt or
/// hostile peer cannot make us allocate unboundedly. Bulk file data is
/// streamed, not framed, so this does not limit file size.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;
pub const MAX_STR: u32 = 4096;
pub const MAX_ARGV: u32 = 1024;

/// Copy chunk for file streaming: large enough for throughput, small enough
/// that several concurrent transfers stay trivially below RAM limits.
pub const COPY_CHUNK: usize = 256 * 1024;

pub fn load_or_create_secret_key(path: &Path) -> Result<SecretKey> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let arr: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .with_context(|| format!("key file {} is not 32 bytes", path.display()))?;
            Ok(SecretKey::from_bytes(&arr))
        }
        Err(_) => {
            let mut arr = [0u8; 32];
            getrandom::getrandom(&mut arr)
                .map_err(|e| anyhow::anyhow!("getrandom for new key: {e}"))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating key dir {}", parent.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
                }
            }
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
                    .with_context(|| format!("creating key file {}", path.display()))?;
                f.write_all(&arr).context("writing key bytes")?;
            }
            #[cfg(not(unix))]
            std::fs::write(path, arr)
                .with_context(|| format!("writing new key to {}", path.display()))?;
            Ok(SecretKey::from_bytes(&arr))
        }
    }
}

// ---------------------------------------------------------------------------
// Framing helpers (LE u32 lengths; see module docs for the layout).
// ---------------------------------------------------------------------------

pub fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

pub struct Cursor<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| anyhow::anyhow!("framed header truncated"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        anyhow::ensure!(n as u32 <= MAX_STR, "string field over {MAX_STR} bytes");
        Ok(String::from_utf8(self.take(n)?.to_vec())?)
    }
    pub fn done(&self) -> Result<()> {
        anyhow::ensure!(self.pos == self.buf.len(), "trailing bytes after header");
        Ok(())
    }
}

/// Write one exec frame: tag byte + u32 len + payload.
pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    tag: u8,
    payload: &[u8],
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut h = [0u8; 5];
    h[0] = tag;
    h[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&h).await?;
    w.write_all(payload).await?;
    Ok(())
}

pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Result<Option<(u8, Vec<u8>)>> {
    use tokio::io::AsyncReadExt;
    let mut h = [0u8; 5];
    match r.read_exact(&mut h).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let tag = h[0];
    let len = u32::from_le_bytes(h[1..].try_into().unwrap());
    anyhow::ensure!(len <= MAX_FRAME, "frame of {len} bytes exceeds {MAX_FRAME}");
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Ok(Some((tag, payload)))
}

/// Read a u8 status followed (on STATUS_ERR) by a u32-len message.
pub async fn read_status<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Result<Result<(), String>> {
    use tokio::io::AsyncReadExt;
    let mut s = [0u8; 1];
    r.read_exact(&mut s).await?;
    match s[0] {
        STATUS_OK => Ok(Ok(())),
        STATUS_ERR => {
            let mut lb = [0u8; 4];
            r.read_exact(&mut lb).await?;
            let n = u32::from_le_bytes(lb);
            anyhow::ensure!(n <= MAX_STR, "error message over {MAX_STR} bytes");
            let mut m = vec![0u8; n as usize];
            r.read_exact(&mut m).await?;
            Ok(Err(String::from_utf8_lossy(&m).into_owned()))
        }
        other => anyhow::bail!("unexpected status byte {other}"),
    }
}

pub async fn write_status_ok<W: tokio::io::AsyncWrite + Unpin>(w: &mut W) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    w.write_all(&[STATUS_OK]).await?;
    Ok(())
}

pub async fn write_status_err<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, msg: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let m = msg.as_bytes();
    w.write_all(&[STATUS_ERR]).await?;
    w.write_all(&(m.len() as u32).to_le_bytes()).await?;
    w.write_all(m).await?;
    Ok(())
}

/// Stream exactly `len` bytes from `r` into `w`, returning the SHA-256 of what
/// moved. Used for both directions of `copy` so the hash trailer can be
/// verified against the bytes as they flowed, not a re-read.
pub async fn stream_hashed<R, W>(r: &mut R, w: &mut W, len: u64) -> Result<[u8; 32]>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut hasher = sha2::Sha256::new();
    let mut remaining = len;
    let mut buf = vec![0u8; COPY_CHUNK.min(len as usize).max(1)];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = r.read(&mut buf[..want]).await?;
        anyhow::ensure!(n > 0, "stream ended {remaining} bytes short of the announced length");
        hasher.update(&buf[..n]);
        w.write_all(&buf[..n]).await?;
        remaining -= n as u64;
    }
    w.flush().await?;
    Ok(hasher.finalize().into())
}

// ---------------------------------------------------------------------------
// Path reporting (spec property 5: never let a relay masquerade as direct).
// ---------------------------------------------------------------------------

/// Classify the connection's currently-selected path. Every accepted/dialed
/// session logs this on both ends; a path upgrade (relay -> direct after
/// holepunch) is reported separately by the agent's path watcher.
pub fn classify_path(conn: &Connection) -> String {
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected()).or_else(|| paths.iter().next());
    match selected {
        Some(p) => classify_transport_addr(p.remote_addr()),
        None => "unknown (no open path yet)".to_string(),
    }
}

pub fn classify_transport_addr(addr: &TransportAddr) -> String {
    match addr {
        TransportAddr::Relay(url) => format!("relayed (relay {url})"),
        TransportAddr::Ip(sock) => {
            let ip = sock.ip();
            if ip.is_loopback() {
                format!("direct-local ({sock})")
            } else if is_private(&ip) {
                format!("direct-LAN ({sock})")
            } else {
                // Beyond the spec's three labels: a direct path over a public
                // IP is possible (no NAT) and must not be mislabeled "LAN".
                format!("direct-public ({sock})")
            }
        }
        other => format!("unknown transport ({other:?})"),
    }
}

fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            // fc00::/7 unique-local and fe80::/10 link-local
            (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn key_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k").join("agent.key");
        let a = load_or_create_secret_key(&p).unwrap();
        let b = load_or_create_secret_key(&p).unwrap();
        assert_eq!(a.public(), b.public(), "second load must reuse the stored key");
        let meta = std::fs::metadata(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn distinct_seeds_distinct_ids() {
        assert_ne!(key(1).public(), key(2).public());
    }

    #[test]
    fn cursor_strings_and_bounds() {
        let mut b = Vec::new();
        put_str(&mut b, "/etc/hostname");
        b.extend_from_slice(&7u64.to_le_bytes());
        let mut c = Cursor::new(&b);
        assert_eq!(c.string().unwrap(), "/etc/hostname");
        assert_eq!(c.u64().unwrap(), 7);
        assert!(c.done().is_ok());
        assert!(Cursor::new(&[0xff, 0xff, 0xff, 0xff]).string().is_err());
        assert!(Cursor::new(&[3, 0, 0, 0, b'a']).string().is_err());
    }

    #[tokio::test]
    async fn frames_roundtrip_and_eof() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_frame(&mut a, frame::STDOUT, b"hello").await.unwrap();
        write_frame(&mut a, frame::EXIT, &3i32.to_le_bytes()).await.unwrap();
        drop(a);
        let (tag, payload) = read_frame(&mut b).await.unwrap().unwrap();
        assert_eq!((tag, payload.as_slice()), (frame::STDOUT, b"hello".as_slice()));
        let (tag, payload) = read_frame(&mut b).await.unwrap().unwrap();
        assert_eq!(tag, frame::EXIT);
        assert_eq!(i32::from_le_bytes(payload.try_into().unwrap()), 3);
        assert!(read_frame(&mut b).await.unwrap().is_none(), "clean EOF, not an error");
    }

    #[tokio::test]
    async fn status_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_status_ok(&mut a).await.unwrap();
        write_status_err(&mut a, "no such file").await.unwrap();
        drop(a);
        assert!(read_status(&mut b).await.unwrap().is_ok());
        assert_eq!(read_status(&mut b).await.unwrap().unwrap_err(), "no such file");
    }

    #[tokio::test]
    async fn stream_hashed_moves_exact_bytes() {
        let data: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        let expect: [u8; 32] = sha2::Sha256::digest(&data).into();
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let src = data.clone();
        let writer = tokio::spawn(async move {
            a.write_all(&src).await.unwrap();
        });
        use tokio::io::AsyncWriteExt;
        let mut out = Vec::new();
        let hash = stream_hashed(&mut b, &mut out, data.len() as u64).await.unwrap();
        writer.await.unwrap();
        assert_eq!(out, data);
        assert_eq!(hash, expect);
    }

    #[tokio::test]
    async fn stream_hashed_rejects_short_stream() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        a.write_all(b"only ten").await.unwrap();
        use tokio::io::AsyncWriteExt;
        drop(a);
        let mut out = Vec::new();
        assert!(stream_hashed(&mut b, &mut out, 100).await.is_err());
    }

    #[test]
    fn classify_ip_labels() {
        let local: TransportAddr = TransportAddr::Ip("127.0.0.1:1234".parse().unwrap());
        assert!(classify_transport_addr(&local).starts_with("direct-local"));
        let lan: TransportAddr = TransportAddr::Ip("192.168.1.5:1234".parse().unwrap());
        assert!(classify_transport_addr(&lan).starts_with("direct-LAN"));
        let wan: TransportAddr = TransportAddr::Ip("8.8.8.8:1234".parse().unwrap());
        assert!(classify_transport_addr(&wan).starts_with("direct-public"));
        let relay: TransportAddr =
            TransportAddr::Relay("https://usw1-1.relay.n0.iroh.link.".parse().unwrap());
        assert!(classify_transport_addr(&relay).starts_with("relayed"));
    }
}
