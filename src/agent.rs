//! The agent: boots on the bare target, dials the controller, serves the three
//! primitives on controller-opened streams. Never listens for inbound
//! connections, never gives up.

use crate::{
    classify_path, load_or_create_secret_key, op, stream_hashed, write_frame, write_status_err,
    write_status_ok, Cursor, ALPN, COPY_CHUNK, MAX_ARGV, MAX_STR,
};
use anyhow::{Context, Result};
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use sha2::Digest;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DIAL_MIN_BACKOFF: Duration = Duration::from_secs(1);
const DIAL_MAX_BACKOFF: Duration = Duration::from_secs(15);

pub struct AgentOpts {
    pub state_dir: PathBuf,
    pub controller: EndpointId,
    pub relay: Option<RelayUrl>,
    pub no_relay: bool,
    pub direct: Vec<SocketAddr>,
}

pub async fn run(opts: AgentOpts) -> Result<()> {
    let key_path = opts.state_dir.join("agent.key");
    let key = load_or_create_secret_key(&key_path)
        .with_context(|| format!("loading agent key {}", key_path.display()))?;
    println!("egdod agent pubkey: {}", key.public());
    println!("egdod agent dialing controller: {}", opts.controller);

    let relay_mode = if opts.no_relay {
        RelayMode::Disabled
    } else if let Some(url) = &opts.relay {
        RelayMode::custom([url.clone()])
    } else {
        RelayMode::Default
    };

    let mut backoff = DIAL_MIN_BACKOFF;
    loop {
        match dial_once(&key, &opts, &relay_mode).await {
            Ok(()) => {
                // Clean close: the controller went away or rejected us.
                println!("egdod agent: session ended; redialing");
            }
            Err(e) => {
                println!("egdod agent: dial failed: {e:#}");
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(DIAL_MAX_BACKOFF);
    }
}

async fn dial_once(key: &SecretKey, opts: &AgentOpts, relay_mode: &RelayMode) -> Result<()> {
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(key.clone())
        .relay_mode(relay_mode.clone());
    // With explicit hints the dial needs no discovery at all — this is what
    // lets an initramfs without working DNS reach the controller (the relay
    // URL may be an IP literal, and --direct is an IP by construction).
    if !opts.direct.is_empty() || opts.relay.is_some() {
        builder = builder.clear_address_lookup();
    }
    let endpoint = builder.bind().await.context("binding agent endpoint")?;

    let mut addr = EndpointAddr::new(opts.controller);
    for a in &opts.direct {
        addr = addr.with_ip_addr(*a);
    }
    if let Some(url) = &opts.relay {
        addr = addr.with_relay_url(url.clone());
    }

    let conn = endpoint
        .connect(addr, ALPN)
        .await
        .context("connecting to controller")?;
    let session = serve_session(&conn);
    let ended = conn.closed();
    tokio::pin!(ended);
    // Serve until the connection dies, whichever the session loop notices
    // first; then close the endpoint so the redial starts from a clean slate.
    let r = tokio::select! {
        r = session => r,
        _ = &mut ended => Ok(()),
    };
    endpoint.close().await;
    r
}

async fn serve_session(conn: &Connection) -> Result<()> {
    println!(
        "egdod agent: connected to controller {} — path: {}",
        conn.remote_id(),
        classify_path(conn)
    );
    // A relayed session that holepunches must not keep wearing its first
    // label: report every selected-path change for the life of the session.
    let watch = conn.clone();
    let watcher = tokio::spawn(async move {
        let mut last = classify_path(&watch);
        let mut stream = Box::pin(watch.paths_stream());
        use futures::StreamExt;
        while stream.next().await.is_some() {
            let now = classify_path(&watch);
            if now != last {
                println!("egdod agent: path changed: {last} -> {now}");
                last = now;
            }
        }
    });

    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => {
                watcher.abort();
                return Ok(());
            }
        };
        tokio::spawn(async move {
            if let Err(e) = handle_stream(send, recv).await {
                eprintln!("egdod agent: stream ended with error: {e:#}");
            }
        });
    }
}

async fn handle_stream(send: SendStream, mut recv: RecvStream) -> Result<()> {
    let mut opb = [0u8; 1];
    // UFCS: the tokio AsyncReadExt version, whose io::Error lets us tell a
    // clean FIN apart from a transport error. (RecvStream's inherent
    // read_exact returns a different error type.)
    if let Err(e) = AsyncReadExt::read_exact(&mut recv, &mut opb).await {
        // A controller-side probe (or a dropped session) can close the stream
        // before sending an op; that is not a protocol violation worth logging.
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(());
        }
        return Err(e.into());
    }
    match opb[0] {
        op::EXEC => handle_exec(send, recv).await,
        op::PUT => handle_put(send, recv).await,
        op::GET => handle_get(send, recv).await,
        op::FWD => handle_fwd(send, recv).await,
        other => anyhow::bail!("unknown op {other}"),
    }
}

/// Header for small fixed-size requests arrives inline; cap the read so a
/// corrupt peer cannot make us buffer unboundedly.
async fn read_header(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>> {
    let mut lb = [0u8; 4];
    recv.read_exact(&mut lb).await?;
    let n = u32::from_le_bytes(lb) as usize;
    anyhow::ensure!(n <= max, "header of {n} bytes exceeds cap {max}");
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn handle_exec(mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let hdr = read_header(&mut recv, (MAX_ARGV * (MAX_STR + 4)) as usize).await?;
    let mut c = Cursor::new(&hdr);
    let argc = c.u32()?;
    anyhow::ensure!(argc >= 1 && argc <= MAX_ARGV, "bad argc {argc}");
    let mut argv = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        argv.push(c.string()?);
    }
    c.done()?;

    // Frame writes from two pipe readers must not interleave: both feed one
    // channel, one writer owns the stream.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(64);
    let writer = tokio::spawn(async move {
        while let Some((tag, payload)) = rx.recv().await {
            write_frame(&mut send, tag, &payload).await?;
        }
        send.finish()?;
        Ok::<(), anyhow::Error>(())
    });

    let spawn = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match spawn {
        Ok(c) => c,
        Err(e) => {
            let _ = tx
                .send((crate::frame::STDERR, format!("spawn {}: {e}", argv[0]).into_bytes()))
                .await;
            // 127: the conventional "command not found", so callers can tell
            // spawn failure apart from the program's own exit codes.
            let _ = tx.send((crate::frame::EXIT, 127i32.to_le_bytes().to_vec())).await;
            drop(tx);
            return writer.await?;
        }
    };

    let mut readers = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            pipe_frames(&mut pipe, crate::frame::STDOUT, tx).await;
        }));
    }
    if let Some(mut pipe) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            pipe_frames(&mut pipe, crate::frame::STDERR, tx).await;
        }));
    }
    for r in readers {
        let _ = r.await;
    }
    let status = child.wait().await?;
    // Negative encodes "killed by signal -n"; the controller reassembles.
    #[cfg(unix)]
    let code: i32 = match status.code() {
        Some(c) => c,
        None => {
            use std::os::unix::process::ExitStatusExt;
            -(status.signal().unwrap_or(0))
        }
    };
    #[cfg(not(unix))]
    let code: i32 = status.code().unwrap_or(-1);
    let _ = tx.send((crate::frame::EXIT, code.to_le_bytes().to_vec())).await;
    drop(tx);
    writer.await?
}

async fn pipe_frames<R: tokio::io::AsyncRead + Unpin>(
    pipe: &mut R,
    tag: u8,
    tx: tokio::sync::mpsc::Sender<(u8, Vec<u8>)>,
) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if tx.send((tag, buf[..n].to_vec())).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

async fn handle_put(mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    match put_inner(&mut recv).await {
        Ok(_msg) => write_status_ok(&mut send).await?,
        Err(e) => write_status_err(&mut send, &format!("{e:#}")).await?,
    }
    send.finish()?;
    Ok(())
}

async fn put_inner(recv: &mut RecvStream) -> Result<String> {
    let hdr = read_header(recv, MAX_STR as usize + 16).await?;
    let mut c = Cursor::new(&hdr);
    let path = c.string()?;
    let mode = c.u32()?;
    let len = c.u64()?;
    c.done()?;

    // Write to a sibling temp file and rename: a failed transfer never leaves
    // a half-written file at the destination path.
    let dest = PathBuf::from(&path);
    let tmp = dest.with_extension("egdod-part");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let hash = stream_hashed(recv, &mut file, len).await;
    let hash = match hash {
        Ok(h) => h,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    let mut trailer = [0u8; 32];
    recv.read_exact(&mut trailer).await?;
    if trailer != hash {
        let _ = tokio::fs::remove_file(&tmp).await;
        anyhow::bail!("sha256 mismatch: announced {}, computed {}", hex(&trailer), hex(&hash));
    }
    file.sync_all().await?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    tokio::fs::rename(&tmp, &dest)
        .await
        .with_context(|| format!("renaming onto {}", dest.display()))?;
    Ok(format!("wrote {} ({} bytes, sha256 {})", dest.display(), len, hex(&hash)))
}

async fn handle_get(mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let hdr = read_header(&mut recv, MAX_STR as usize + 4).await?;
    let mut c = Cursor::new(&hdr);
    let path = c.string()?;
    c.done()?;

    match get_inner(&mut send, &path).await {
        Ok(()) => {}
        Err(e) => {
            // If the error happened after the OK header went out the transfer
            // is already committed; the hash trailer is the caller's guard.
            write_status_err(&mut send, &format!("{e:#}")).await.ok();
        }
    }
    send.finish()?;
    Ok(())
}

async fn get_inner(send: &mut SendStream, path: &str) -> Result<()> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {path}"))?;
    anyhow::ensure!(meta.is_file(), "{path} is not a regular file");
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    };
    #[cfg(not(unix))]
    let mode = 0o644u32;
    let len = meta.len();

    let mut hdr = Vec::with_capacity(13);
    hdr.extend_from_slice(&mode.to_le_bytes());
    hdr.extend_from_slice(&len.to_le_bytes());
    write_status_ok(send).await?;
    send.write_all(&hdr).await?;

    let mut file = tokio::fs::File::open(path).await.with_context(|| format!("open {path}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut remaining = len;
    let mut buf = vec![0u8; COPY_CHUNK];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want]).await?;
        anyhow::ensure!(n > 0, "{path} shrank mid-read ({remaining} bytes short)");
        hasher.update(&buf[..n]);
        send.write_all(&buf[..n]).await?;
        remaining -= n as u64;
    }
    let hash: [u8; 32] = hasher.finalize().into();
    send.write_all(&hash).await?;
    Ok(())
}

async fn handle_fwd(mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let hdr = read_header(&mut recv, MAX_STR as usize + 4).await?;
    let mut c = Cursor::new(&hdr);
    let addr = c.string()?;
    c.done()?;

    let tcp = match tokio::net::TcpStream::connect(&addr).await {
        Ok(t) => t,
        Err(e) => {
            write_status_err(&mut send, &format!("connect {addr}: {e:#}")).await?;
            send.finish()?;
            return Ok(());
        }
    };
    write_status_ok(&mut send).await?;
    pump(tcp, send, recv).await
}

/// Bidirectional splice between a local TCP socket and an iroh stream.
pub async fn pump(tcp: tokio::net::TcpStream, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut tcp_r, &mut send).await;
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_w).await;
        let _ = tcp_w.shutdown().await;
    };
    tokio::join!(up, down);
    Ok(())
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
