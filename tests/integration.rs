//! End-to-end over a real iroh connection on loopback: controller serve + an
//! agent dialing it by NodeId with a direct hint, approval gate, then all
//! three primitives through the control-socket bridge.

use egdod::{agent, cmd, serve};
use iroh::EndpointId;
use sha2::Digest;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

async fn wait_for<F: FnMut() -> bool>(mut f: F, what: &str) {
    let deadline = std::time::Instant::now() + T;
    while !f() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn end_to_end_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let ctl = dir.path().join("ctl");
    let agt = dir.path().join("agt");

    let running = serve::bind(serve::ServeOpts {
        state_dir: ctl.clone(),
        relay: None,
        no_relay: true,
    })
    .await
    .unwrap();
    let id: EndpointId = running.endpoint.id();
    let port = running
        .endpoint
        .addr()
        .ip_addrs()
        .next()
        .expect("controller has a bound addr")
        .port();
    let direct: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let agents = running.agents.clone();
    let bridge = tokio::spawn(serve::bridge_listener(ctl.clone(), agents));

    let agent_task = tokio::spawn(agent::run(agent::AgentOpts {
        state_dir: agt.clone(),
        controller: id,
        relay: None,
        no_relay: true,
        direct: vec![direct],
    }));

    // The agent's key is generated on first run, and its public half is all
    // the controller ever learns — nothing secret crosses either way.
    let agent_key = egdod::load_or_create_secret_key(&agt.join("agent.key")).unwrap();
    let agent_pk = agent_key.public();

    // 1. Unapproved: recorded pending, gets nothing.
    wait_for(
        || serve::list_pending(&ctl).unwrap().iter().any(|p| p.pubkey == agent_pk.to_string()),
        "agent to appear in pending",
    )
    .await;
    assert!(
        cmd::connect_agent(&ctl, &agent_pk).await.is_err(),
        "unapproved agent must not be reachable through the bridge"
    );

    // 2. Approve; the agent's next redial registers.
    serve::approve(&ctl, &agent_pk).unwrap();
    assert!(serve::list_pending(&ctl).unwrap().is_empty(), "approval clears pending");
    {
        let deadline = std::time::Instant::now() + T;
        loop {
            if cmd::connect_agent(&ctl, &agent_pk).await.is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "agent never registered after approval");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    // 3. exec: stdout/stderr kept apart, exit status returned.
    let mut s = cmd::connect_agent(&ctl, &agent_pk).await.unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cmd::exec_with(
        &mut s,
        &["/bin/sh".into(), "-c".into(), "echo hello-out; echo hello-err >&2; exit 3".into()],
        &mut out,
        &mut err,
    )
    .await
    .unwrap();
    assert_eq!(code, 3);
    assert_eq!(String::from_utf8(out).unwrap(), "hello-out\n");
    assert_eq!(String::from_utf8(err).unwrap(), "hello-err\n");

    // Spawn failure is distinguishable from the program's own exit codes.
    let mut s = cmd::connect_agent(&ctl, &agent_pk).await.unwrap();
    let code = cmd::exec_with(&mut s, &["/nonexistent-egdod".into()], &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap();
    assert_eq!(code, 127);

    // 4. copy both directions: 5 MiB, mode preserved, sha256 verified in
    // both protocol directions (the trailer check runs inside push/pull).
    let payload: Vec<u8> = (0..5 * 1024 * 1024u32).map(|i| (i * 7 % 251) as u8).collect();
    let local_up = dir.path().join("up.bin");
    std::fs::write(&local_up, &payload).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&local_up, std::fs::Permissions::from_mode(0o751)).unwrap();
    }
    let remote = agt.join("remote.bin");
    let mut s = cmd::connect_agent(&ctl, &agent_pk).await.unwrap();
    cmd::push(&mut s, &local_up, remote.to_str().unwrap()).await.unwrap();

    let local_down = dir.path().join("down.bin");
    let mut s = cmd::connect_agent(&ctl, &agent_pk).await.unwrap();
    cmd::pull(&mut s, remote.to_str().unwrap(), &local_down).await.unwrap();
    assert_eq!(
        sha2::Sha256::digest(std::fs::read(&local_down).unwrap()),
        sha2::Sha256::digest(&payload),
        "round-tripped content must hash identically"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&local_down).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o751, "mode must survive the round trip");
    }

    // 5. forward: bridge a local listener onto a TCP service "on the target".
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = echo.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                tokio::io::copy(&mut r, &mut w).await.ok();
            });
        }
    });
    let fwd = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fwd_port = fwd.local_addr().unwrap().port();
    let fwd_task = tokio::spawn(cmd::forward_on(
        fwd,
        ctl.clone(),
        agent_pk,
        format!("127.0.0.1:{echo_port}"),
    ));

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut c = tokio::net::TcpStream::connect(format!("127.0.0.1:{fwd_port}")).await.unwrap();
    c.write_all(b"ping-through-iroh").await.unwrap();
    let mut buf = vec![0u8; 17];
    c.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, b"ping-through-iroh");

    // 6. Every session reports its path; with a loopback hint iroh may still
    // select another local interface, but it must never read as relayed.
    let path = {
        let g = running.agents.lock().await;
        let (_, conn) = g.get(&agent_pk).expect("agent still registered");
        egdod::classify_path(conn)
    };
    assert!(path.starts_with("direct-"), "unexpected path label: {path}");

    fwd_task.abort();
    bridge.abort();
    agent_task.abort();
}
