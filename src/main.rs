use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use iroh::{EndpointId, PublicKey, RelayUrl};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(about = "egdod: wake a machine with no OS — exec, copy, forward over one dialed-out iroh connection")]
enum Cli {
    Controller {
        #[command(subcommand)]
        cmd: ControllerCmd,
    },
    Agent(AgentArgs),
}

#[derive(Args)]
struct AgentArgs {
    /// The controller's NodeId — public, the only thing baked into the image.
    #[arg(long)]
    controller: EndpointId,
    /// Use only this relay (an IP-literal URL needs no DNS).
    #[arg(long, conflicts_with = "no_relay")]
    relay: Option<RelayUrl>,
    /// No relay at all: direct paths only (LAN/cable, or --direct).
    #[arg(long)]
    no_relay: bool,
    /// Direct address hint for the controller; repeatable. With hints (or
    /// --relay), discovery/DNS is not needed at all.
    #[arg(long = "direct")]
    direct: Vec<SocketAddr>,
    /// Where the agent keeps its key. In an initramfs, point this at
    /// persistent storage or the identity (and its approval) is per-boot.
    #[arg(long, default_value = "/var/lib/egdod-agent")]
    state_dir: PathBuf,
}

#[derive(Subcommand)]
enum ControllerCmd {
    /// Create the controller key; print the NodeId.
    Init {
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
    },
    /// Accept agents; hold unapproved ones pending.
    Serve {
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
        #[arg(long, conflicts_with = "no_relay")]
        relay: Option<RelayUrl>,
        #[arg(long)]
        no_relay: bool,
    },
    /// List agents awaiting approval.
    Pending {
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
        /// Machine-readable output (spec property 6: the controller is driven
        /// by a program, not a person).
        #[arg(long)]
        json: bool,
    },
    /// Approve an agent by public key. Scriptable; survives restart.
    Approve {
        pubkey: PublicKey,
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
    },
    /// Run argv on the target as root; stream stdout/stderr; exit status is
    /// the remote exit status.
    Exec {
        agent: PublicKey,
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Copy a local file to the target (streams; sha256-verified; keeps mode).
    Push {
        agent: PublicKey,
        local: PathBuf,
        remote: String,
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
    },
    /// Copy a file from the target (streams; sha256-verified; keeps mode).
    Pull {
        agent: PublicKey,
        remote: String,
        local: PathBuf,
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
    },
    /// Tunnel a local TCP listener to addr:port on the target.
    Forward {
        agent: PublicKey,
        local: String,
        remote: String,
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
    },
    /// ssh, composed from exec + copy + forward (no special transport).
    Ssh {
        agent: PublicKey,
        /// Local port for the forward; 0 = pick a free one.
        #[arg(long, default_value = "0")]
        port: u16,
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Print controller health: reachability canary + approval counts.
    /// (Not in the spec's command surface; it is how the spec's "expose the
    /// state" requirement is made usable — see PROOF.md.)
    Status {
        #[arg(long, default_value = default_state_dir())]
        state_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

fn default_state_dir() -> &'static str {
    // Leak-free enough for a CLI default parsed once.
    match std::env::var("XDG_STATE_HOME") {
        Ok(x) if !x.is_empty() => Box::leak(format!("{x}/egdod").into_boxed_str()),
        _ => match std::env::var("HOME") {
            Ok(h) => Box::leak(format!("{h}/.local/state/egdod").into_boxed_str()),
            Err(_) => "./egdod-state",
        },
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    match run().await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("egdod: error: {e:#}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<u8> {
    match Cli::parse() {
        Cli::Agent(a) => {
            egdod::agent::run(egdod::agent::AgentOpts {
                state_dir: a.state_dir,
                controller: a.controller,
                relay: a.relay,
                no_relay: a.no_relay,
                direct: a.direct,
            })
            .await?;
            Ok(0)
        }
        Cli::Controller { cmd } => controller(cmd).await,
    }
}

async fn controller(cmd: ControllerCmd) -> Result<u8> {
    match cmd {
        ControllerCmd::Init { state_dir } => {
            let key = egdod::load_or_create_secret_key(&state_dir.join("controller.key"))?;
            println!("{}", key.public());
            Ok(0)
        }
        ControllerCmd::Serve {
            state_dir,
            relay,
            no_relay,
        } => {
            egdod::serve::run(egdod::serve::ServeOpts {
                state_dir,
                relay,
                no_relay,
            })
            .await?;
            Ok(0)
        }
        ControllerCmd::Pending { state_dir, json } => {
            let pending = egdod::serve::list_pending(&state_dir)?;
            if json {
                let v: Vec<_> = pending
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "pubkey": p.pubkey,
                            "first_seen": p.first_seen,
                            "last_seen": p.last_seen,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"pending": v})).unwrap());
            } else {
                for p in &pending {
                    println!("{}\tfirst_seen={}\tlast_seen={}", p.pubkey, p.first_seen, p.last_seen);
                }
            }
            Ok(0)
        }
        ControllerCmd::Approve { pubkey, state_dir } => {
            let was_pending = egdod::serve::approve(&state_dir, &pubkey)?;
            if was_pending {
                println!("approved {pubkey} (was pending)");
            } else {
                // Pre-approving a known key before its first dial is the
                // scripted-provisioning path, so it is not an error.
                println!("approved {pubkey} (not seen yet; will apply on first dial)");
            }
            Ok(0)
        }
        ControllerCmd::Exec {
            agent,
            state_dir,
            argv,
        } => {
            let mut s = egdod::cmd::connect_agent(&state_dir, &agent).await?;
            let code = egdod::cmd::exec(&mut s, &argv).await?;
            if code < 0 {
                eprintln!("egdod exec: remote killed by signal {}", -code);
                Ok(128 + (-code).min(127) as u8)
            } else {
                Ok(code.min(255) as u8)
            }
        }
        ControllerCmd::Push {
            agent,
            local,
            remote,
            state_dir,
        } => {
            let mut s = egdod::cmd::connect_agent(&state_dir, &agent).await?;
            let n = egdod::cmd::push(&mut s, &local, &remote).await?;
            eprintln!("pushed {} bytes to {remote}", n);
            Ok(0)
        }
        ControllerCmd::Pull {
            agent,
            remote,
            local,
            state_dir,
        } => {
            let mut s = egdod::cmd::connect_agent(&state_dir, &agent).await?;
            let n = egdod::cmd::pull(&mut s, &remote, &local).await?;
            eprintln!("pulled {} bytes from {remote}", n);
            Ok(0)
        }
        ControllerCmd::Forward {
            agent,
            local,
            remote,
            state_dir,
        } => {
            egdod::cmd::forward(&state_dir, &agent, &local, &remote).await?;
            Ok(0)
        }
        ControllerCmd::Ssh {
            agent,
            port,
            state_dir,
            cmd,
        } => {
            let code = egdod::cmd::ssh(egdod::cmd::SshOpts {
                state_dir,
                agent,
                local_port: port,
                remote_cmd: cmd,
            })
            .await?;
            Ok(code.min(255) as u8)
        }
        ControllerCmd::Status { state_dir, json } => {
            let reach = std::fs::read_to_string(state_dir.join("reachability.json"))
                .context("no reachability.json — is `egdod controller serve` running?")?;
            let reach: serde_json::Value = serde_json::from_str(&reach)?;
            let approved = std::fs::read_dir(state_dir.join("approved"))
                .map(|d| d.count())
                .unwrap_or(0);
            let pending = egdod::serve::list_pending(&state_dir)?.len();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "reachability": reach,
                        "approved": approved,
                        "pending": pending,
                    }))
                    .unwrap()
                );
            } else {
                let ok = reach.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    println!(
                        "dialable: yes (checked_at={}, path={})",
                        reach.get("checked_at").and_then(|v| v.as_u64()).unwrap_or(0),
                        reach.get("path").and_then(|v| v.as_str()).unwrap_or("?"),
                    );
                } else {
                    println!(
                        "dialable: NO — {}",
                        reach.get("error").and_then(|v| v.as_str()).unwrap_or("?")
                    );
                }
                println!("approved agents: {approved}; pending: {pending}");
            }
            Ok(if reach.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                0
            } else {
                2
            })
        }
    }
}
