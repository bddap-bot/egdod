//! Controller one-shot commands. Each connects to the running `serve` over its
//! control socket, which bridges the local stream onto a fresh stream to the
//! named agent; from there the command speaks the same protocol the agent
//! serves. Protocol knowledge lives here and in agent.rs — serve only routes.

use crate::{op, put_str, read_frame, read_status, stream_hashed, Cursor};
use anyhow::{Context, Result};
use iroh::PublicKey;
use sha2::Digest;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};

/// Connect to the running controller's control socket and open a stream to
/// `agent`. The returned stream is positioned at the start of protocol bytes.
pub async fn connect_agent(state_dir: &Path, agent: &PublicKey) -> Result<UnixStream> {
    let sock = UnixStream::connect(state_dir.join("control.sock"))
        .await
        .context("connecting to controller control socket (is `egdod controller serve` running?)")?;
    let (mut r, mut w) = tokio::io::split(sock);
    w.write_all(format!("connect {agent}\n").as_bytes()).await?;
    let mut reply = String::new();
    {
        use tokio::io::AsyncBufReadExt;
        let mut br = tokio::io::BufReader::new(&mut r);
        br.read_line(&mut reply).await?;
    }
    match reply.trim_end() {
        "ok" => {}
        other => anyhow::bail!("controller: {}", other.strip_prefix("err ").unwrap_or(other)),
    }
    Ok(r.unsplit(w))
}

fn header(body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + body.len());
    b.extend_from_slice(&(body.len() as u32).to_le_bytes());
    b.extend_from_slice(body);
    b
}

// ---------------------------------------------------------------------------
// exec
// ---------------------------------------------------------------------------

/// Run argv on the agent; stream stdout/stderr frames to our own as they
/// arrive. Returns the remote exit code (negative = killed by signal -n).
pub async fn exec<S>(stream: &mut S, argv: &[String]) -> Result<i32>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut out = tokio::io::stdout();
    let mut err = tokio::io::stderr();
    exec_with(stream, argv, &mut out, &mut err).await
}

pub async fn exec_with<S, O, E>(
    stream: &mut S,
    argv: &[String],
    out: &mut O,
    err: &mut E,
) -> Result<i32>
where
    S: AsyncRead + AsyncWrite + Unpin,
    O: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
{
    anyhow::ensure!(!argv.is_empty(), "empty argv");
    let mut body = Vec::new();
    body.extend_from_slice(&(argv.len() as u32).to_le_bytes());
    for a in argv {
        put_str(&mut body, a);
    }
    stream.write_all(&[op::EXEC]).await?;
    stream.write_all(&header(&body)).await?;

    loop {
        let frame = read_frame(stream)
            .await?
            .context("stream ended before the exit frame")?;
        match frame {
            (crate::frame::STDOUT, payload) => {
                out.write_all(&payload).await?;
                out.flush().await?;
            }
            (crate::frame::STDERR, payload) => {
                err.write_all(&payload).await?;
                err.flush().await?;
            }
            (crate::frame::EXIT, payload) => {
                anyhow::ensure!(payload.len() == 4, "bad exit frame");
                return Ok(i32::from_le_bytes(payload.try_into().unwrap()));
            }
            (tag, _) => anyhow::bail!("unexpected exec frame tag {tag}"),
        }
    }
}

// ---------------------------------------------------------------------------
// copy (push / pull)
// ---------------------------------------------------------------------------

pub async fn push<S>(stream: &mut S, local: &Path, remote: &str) -> Result<u64>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let meta = std::fs::metadata(local)
        .with_context(|| format!("stat {}", local.display()))?;
    anyhow::ensure!(meta.is_file(), "{} is not a regular file", local.display());
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    };
    #[cfg(not(unix))]
    let mode = 0o644u32;
    let len = meta.len();

    let mut body = Vec::new();
    put_str(&mut body, remote);
    body.extend_from_slice(&mode.to_le_bytes());
    body.extend_from_slice(&len.to_le_bytes());
    stream.write_all(&[op::PUT]).await?;
    stream.write_all(&header(&body)).await?;

    // Stream the file and append the sha256 trailer; the agent verifies it
    // against the bytes as they arrived before renaming into place.
    let mut file = tokio::fs::File::open(local).await?;
    let mut hasher = sha2::Sha256::new();
    let mut remaining = len;
    let mut buf = vec![0u8; crate::COPY_CHUNK];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want]).await?;
        anyhow::ensure!(n > 0, "{} shrank mid-send", local.display());
        hasher.update(&buf[..n]);
        stream.write_all(&buf[..n]).await?;
        remaining -= n as u64;
    }
    let hash: [u8; 32] = hasher.finalize().into();
    stream.write_all(&hash).await?;
    stream.flush().await?;

    match read_status(stream).await? {
        Ok(()) => Ok(len),
        Err(e) => anyhow::bail!("agent rejected the file: {e}"),
    }
}

pub async fn pull<S>(stream: &mut S, remote: &str, local: &Path) -> Result<u64>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut body = Vec::new();
    put_str(&mut body, remote);
    stream.write_all(&[op::GET]).await?;
    stream.write_all(&header(&body)).await?;

    match read_status(stream).await? {
        Ok(()) => {}
        Err(e) => anyhow::bail!("agent: {e}"),
    }
    let mut hdr = [0u8; 12];
    stream.read_exact(&mut hdr).await?;
    let mut c = Cursor::new(&hdr);
    let mode = c.u32()?;
    let len = c.u64()?;

    let tmp = local.with_extension("egdod-part");
    let mut file = tokio::fs::File::create(&tmp).await?;
    let hash = stream_hashed(stream, &mut file, len).await;
    let hash = match hash {
        Ok(h) => h,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    let mut trailer = [0u8; 32];
    stream.read_exact(&mut trailer).await?;
    if trailer != hash {
        let _ = tokio::fs::remove_file(&tmp).await;
        anyhow::bail!("sha256 mismatch on pull");
    }
    file.sync_all().await?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    tokio::fs::rename(&tmp, local).await?;
    Ok(len)
}

// ---------------------------------------------------------------------------
// forward
// ---------------------------------------------------------------------------

/// Serve a local TCP listener; each accepted connection is bridged onto a
/// fresh stream to the agent, which connects `remote` on the target side.
pub async fn forward(state_dir: &Path, agent: &PublicKey, listen: &str, remote: &str) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding local listener {listen}"))?;
    println!("egdod forward: {listen} -> {remote} on agent {agent}");
    forward_on(listener, state_dir.to_path_buf(), *agent, remote.to_string()).await
}

pub async fn forward_on(
    listener: TcpListener,
    state_dir: PathBuf,
    agent: PublicKey,
    remote: String,
) -> Result<()> {
    loop {
        let (tcp, _) = listener.accept().await?;
        let state_dir = state_dir.clone();
        let remote = remote.clone();
        tokio::spawn(async move {
            if let Err(e) = forward_one(tcp, &state_dir, &agent, &remote).await {
                eprintln!("egdod forward: connection ended: {e:#}");
            }
        });
    }
}

async fn forward_one(
    mut tcp: tokio::net::TcpStream,
    state_dir: &Path,
    agent: &PublicKey,
    remote: &str,
) -> Result<()> {
    let mut stream = connect_agent(state_dir, agent).await?;
    let mut body = Vec::new();
    put_str(&mut body, remote);
    stream.write_all(&[op::FWD]).await?;
    stream.write_all(&header(&body)).await?;
    match read_status(&mut stream).await? {
        Ok(()) => {}
        Err(e) => anyhow::bail!("agent: {e}"),
    }
    let (mut tr, mut tw) = tcp.split();
    let (mut sr, mut sw) = tokio::io::split(stream);
    let up = async { let _ = tokio::io::copy(&mut tr, &mut sw).await; };
    let down = async { let _ = tokio::io::copy(&mut sr, &mut tw).await; };
    tokio::join!(up, down);
    Ok(())
}

// ---------------------------------------------------------------------------
// ssh: a thin composition of exec + copy + forward. No special transport —
// everything below is spelled in the three primitives.
// ---------------------------------------------------------------------------

pub struct SshOpts {
    pub state_dir: PathBuf,
    pub agent: PublicKey,
    pub local_port: u16,
    pub remote_cmd: Vec<String>,
}

pub async fn ssh(opts: SshOpts) -> Result<i32> {
    let ssh_dir = opts.state_dir.join("ssh");
    std::fs::create_dir_all(&ssh_dir)?;
    let key_path = ssh_dir.join("id_ed25519");
    if !key_path.exists() {
        // The recipe's last step is the local ssh client, so depending on
        // openssh on the controller adds no new assumption.
        let st = std::process::Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-q", "-f"])
            .arg(&key_path)
            .status()
            .context("running ssh-keygen")?;
        anyhow::ensure!(st.success(), "ssh-keygen failed: {st}");
    }
    let pubkey_line = std::fs::read_to_string(format!("{}.pub", key_path.display()))?;
    let pubkey_line = pubkey_line.trim_end().to_string();

    let agent = opts.agent;
    let state_dir = opts.state_dir.clone();

    // One op per stream — the agent serves exactly one primitive invocation
    // per controller-opened stream, so every step opens a fresh one.
    let exec_on = |argv: &[String]| {
        let state_dir = state_dir.clone();
        let argv = argv.to_vec();
        async move {
            let mut s = connect_agent(&state_dir, &agent).await?;
            exec(&mut s, &argv).await
        }
    };

    // 1. Install the authorized key (exec + pull/push — merge, never clobber).
    exec_on(&["mkdir".into(), "-p".into(), "/root/.ssh".into()]).await?;
    exec_on(&["chmod".into(), "700".into(), "/root/.ssh".into()]).await?;
    let mut merged = match pull_to_memory(&state_dir, &agent, "/root/.ssh/authorized_keys").await {
        Ok(existing) => String::from_utf8_lossy(&existing).into_owned(),
        Err(_) => String::new(),
    };
    if !merged.lines().any(|l| l.trim() == pubkey_line) {
        merged.push_str(&pubkey_line);
        merged.push('\n');
    }
    let tmp = ssh_dir.join("authorized_keys");
    std::fs::write(&tmp, &merged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    let mut s2 = connect_agent(&state_dir, &agent).await?;
    push(&mut s2, &tmp, "/root/.ssh/authorized_keys").await?;

    // 2. Ensure an sshd is up: `sshd -t` validates binary + config + host
    // keys. If it fails we try to generate host keys and start sshd. A target
    // with no sshd at all is out of the recipe's scope (see PROOF.md).
    let probe = exec_on(&["sshd".into(), "-t".into()]).await?;
    if probe != 0 {
        let _ = exec_on(&["ssh-keygen".into(), "-A".into()]).await?;
        let started = exec_on(&["sshd".into()]).await?;
        anyhow::ensure!(started == 0, "could not start sshd on the target (exit {started})");
    }

    // 3. Fetch the host public key over the already-authenticated channel —
    // this is what makes the first ssh connect verifiable, not trusted.
    let host_pub = pull_to_memory(&state_dir, &agent, "/etc/ssh/ssh_host_ed25519_key.pub")
        .await
        .context("pulling target sshd host public key")?;
    let host_line = String::from_utf8_lossy(&host_pub).trim_end().to_string();
    let mut fields = host_line.split_whitespace();
    let (ktype, kblob) = (fields.next(), fields.next());
    let (ktype, kblob) = match (ktype, kblob) {
        (Some(t), Some(b)) => (t, b),
        _ => anyhow::bail!("unparsable host key line: {host_line:?}"),
    };

    // 4. Forward a local port to the target's sshd.
    let listener = TcpListener::bind(("127.0.0.1", opts.local_port)).await?;
    let port = listener.local_addr()?.port();
    let fwd_state = state_dir.clone();
    let fwd_agent = agent;
    tokio::spawn(async move {
        let _ = forward_on(listener, fwd_state, fwd_agent, "127.0.0.1:22".to_string()).await;
    });

    // 5. known_hosts from the in-band host key, then strict batch ssh.
    let kh = ssh_dir.join("known_hosts");
    std::fs::write(&kh, format!("[127.0.0.1]:{port} {ktype} {kblob}\n"))?;
    println!("egdod ssh: forward 127.0.0.1:{port} -> 127.0.0.1:22 on {agent}");
    let mut cmd = std::process::Command::new("ssh");
    cmd.args([
        "-o", "StrictHostKeyChecking=yes",
        "-o", "BatchMode=yes",
        "-o", "IdentitiesOnly=yes",
        "-o", &format!("UserKnownHostsFile={}", kh.display()),
        "-i",
    ])
    .arg(&key_path)
    .args(["-p", &port.to_string(), "root@127.0.0.1"])
    .args(&opts.remote_cmd);
    let st = cmd.status().context("spawning ssh client")?;
    Ok(st.code().unwrap_or(255))
}

/// pull into memory — for small recipe files (authorized_keys, host .pub).
/// Bulk data goes through `pull` proper, which streams to disk.
async fn pull_to_memory(state_dir: &Path, agent: &PublicKey, remote: &str) -> Result<Vec<u8>> {
    let mut s = connect_agent(state_dir, agent).await?;
    let mut body = Vec::new();
    put_str(&mut body, remote);
    s.write_all(&[op::GET]).await?;
    s.write_all(&header(&body)).await?;
    match read_status(&mut s).await? {
        Ok(()) => {}
        Err(e) => anyhow::bail!("agent: {e}"),
    }
    let mut hdr = [0u8; 12];
    s.read_exact(&mut hdr).await?;
    let mut c = Cursor::new(&hdr);
    let _mode = c.u32()?;
    let len = c.u64()?;
    anyhow::ensure!(len <= crate::MAX_FRAME as u64, "{remote} too large for an in-memory pull");
    let mut data = Vec::new();
    let hash = stream_hashed(&mut s, &mut data, len).await?;
    let mut trailer = [0u8; 32];
    s.read_exact(&mut trailer).await?;
    anyhow::ensure!(trailer == hash, "sha256 mismatch pulling {remote}");
    Ok(data)
}
