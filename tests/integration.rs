//! The whole path, end to end, over a real iroh connection on loopback: a
//! controller serving, an agent dialling it by node id, the approval gate
//! refusing and then admitting it, and all three primitives driven the way the
//! CLI drives them — through the control socket, not through internals.
//!
//! `demo.sh` covers more (a real relay, discovery, the ssh recipe), but it needs
//! sshd and a network and it is not `cargo test`. Everything here runs offline
//! in a tempdir, so the parts most likely to rot — routing, the gate, the three
//! primitives — break the build rather than the demo.

use egdod::state::StateDir;
use egdod::{agent, controller};
use egdod::net::RelayChoice;
use sha2::Digest;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(30);

/// Polls until `f` returns something, which is how you wait on a distributed
/// state change without inventing a synchronisation channel just for a test.
async fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[tokio::test(flavor = "multi_thread")]
async fn controller_and_agent_over_iroh() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("controller"));
    state.ensure().unwrap();
    let node_id = state.init_key().unwrap().public();

    // No relay: the test must not depend on n0's infrastructure, and with a
    // direct hint the agent does not need it. That also puts the canary in
    // Direct mode, which is what the status assertions below expect.
    let serving = tokio::spawn(controller::serve(controller::ServeConfig {
        state: state.clone(),
        relay: RelayChoice::Disabled,
        probe_interval: Duration::from_secs(1),
        probe_timeout: Duration::from_secs(5),
        restart_after: 1000,
        bind_addr: Some("127.0.0.1:0".parse().unwrap()),
    }));

    // The controller publishes the addresses it actually bound, which is the
    // only way to learn the ephemeral port it was given.
    let hint: SocketAddr = eventually("the controller to publish a direct address", || {
        let s = state.read_status().ok()?;
        s.direct_addrs.iter().find_map(|a| a.parse().ok())
    })
    .await;

    let key_file = dir.path().join("agent/agent.key");
    let agent_task = tokio::spawn(agent::run(agent::Config {
        controller: node_id,
        key_file: key_file.clone(),
        relay: RelayChoice::Disabled,
        direct: vec![hint],
    }));

    // ---- the gate: pending, and given nothing ----
    let pending = eventually("the agent to be recorded pending", || {
        state.pending().ok()?.into_iter().next()
    })
    .await;
    let agent_pk = pending.pubkey.clone();
    let agent_key = egdod::proto::parse_pubkey(&pending.pubkey).unwrap();

    for attempt in [
        controller::exec(&state, &agent_pk, vec!["/bin/true".into()])
            .await
            .err(),
        controller::push(&state, &agent_pk, &key_file, "/tmp/should-not-land")
            .await
            .err(),
        controller::pull(&state, &agent_pk, "/etc/hostname", &dir.path().join("nope"))
            .await
            .err(),
    ] {
        let e = attempt.expect("an unapproved agent must not be reachable at all");
        assert!(
            format!("{e:#}").contains("no agent is connected"),
            "unapproved agent leaked something other than a routing refusal: {e:#}"
        );
    }
    assert!(!dir.path().join("nope").exists());

    // ---- approve, and it becomes reachable without restarting anything ----
    assert!(state.approve(&agent_key).unwrap(), "first approval");
    assert!(
        state.pending().unwrap().is_empty(),
        "approval clears the pending entry"
    );
    eventually("the approved agent to connect", || {
        (!state.read_status().ok()?.sessions.is_empty()).then_some(())
    })
    .await;

    // ---- exec: streams kept apart, exit status returned ----
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = controller::exec_into(
        &state,
        &agent_pk,
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo to-stdout; echo to-stderr >&2; exit 3".into(),
        ],
        &mut out,
        &mut err,
    )
    .await
    .unwrap();
    assert_eq!(code, 3);
    assert_eq!(String::from_utf8(out).unwrap(), "to-stdout\n");
    assert_eq!(String::from_utf8(err).unwrap(), "to-stderr\n");

    // A command that cannot be spawned is an error, not an exit status, so it
    // can never be mistaken for the command having run and failed.
    let failed = controller::exec(&state, &agent_pk, vec!["/no-such-program".into()])
        .await
        .expect_err("spawning a missing program must not report an exit status");
    assert!(format!("{failed:#}").contains("exec failed on the target"));

    // ---- copy: both directions, larger than one chunk, mode preserved ----
    let payload: Vec<u8> = (0..5 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let up = dir.path().join("up.bin");
    std::fs::write(&up, &payload).unwrap();
    std::fs::set_permissions(&up, {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(0o751)
    })
    .unwrap();

    let on_target = dir.path().join("target-side.bin");
    let on_target_s = on_target.to_str().unwrap();
    controller::push(&state, &agent_pk, &up, on_target_s)
        .await
        .unwrap();
    assert_eq!(mode_of(&on_target), 0o751, "mode survived the push");

    let down = dir.path().join("down.bin");
    controller::pull(&state, &agent_pk, on_target_s, &down)
        .await
        .unwrap();
    assert_eq!(
        sha2::Sha256::digest(std::fs::read(&down).unwrap()),
        sha2::Sha256::digest(&payload),
        "the round trip changed the bytes"
    );
    assert_eq!(mode_of(&down), 0o751, "mode survived the pull");

    // Pulling something that is not there is distinct from pulling something
    // that failed, and leaves no file behind either way.
    let missing = dir.path().join("missing.bin");
    let e = controller::pull(&state, &agent_pk, "/no/such/file", &missing)
        .await
        .expect_err("pulling a missing path must fail");
    assert!(
        format!("{e:#}").contains("has no /no/such/file"),
        "a missing path must be reported as missing, not as a generic failure: {e:#}"
    );
    assert!(!missing.exists());

    // ---- forward: a local port carries bytes to a service on the target ----
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let (bound, forwarding) = controller::forward_listener(
        &state,
        &agent_pk,
        "127.0.0.1:0".parse().unwrap(),
        echo_addr.to_string(),
    )
    .await
    .unwrap();

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut c = tokio::net::TcpStream::connect(bound).await.unwrap();
    c.write_all(b"through-the-tunnel").await.unwrap();
    let mut buf = [0u8; 18];
    c.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"through-the-tunnel");

    // ---- every session says which path it is on, and it is not a guess ----
    let sessions = state.read_status().unwrap().sessions;
    let s = sessions.first().expect("the session is still published");
    assert_eq!(s.agent, agent_pk);
    assert_eq!(
        s.path.to_string(),
        "direct-local",
        "a loopback session reported as something else"
    );

    forwarding.abort();
    agent_task.abort();
    serving.abort();
}
