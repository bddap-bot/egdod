use anyhow::{Context, Result};
use std::io::IsTerminal;
use clap::{Args, Parser, Subcommand};
use egdod::controller::{self, ServeConfig};
use egdod::net::RelayChoice;
use egdod::proto::parse_pubkey;
use egdod::state::StateDir;
use egdod::{agent, ssh};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "egdod", version, about = "Dial-out root access to a bare machine over iroh")]
struct Cli {
    #[command(subcommand)]
    role: Role,
}

#[derive(Subcommand)]
enum Role {
    /// Runs where you are; owns the key, approves agents, drives the primitives.
    Controller {
        /// All controller state lives here and nowhere else, so moving a
        /// controller is copying this directory.
        #[arg(long, global = true)]
        state_dir: Option<PathBuf>,
        #[command(subcommand)]
        cmd: ControllerCmd,
    },
    /// Runs on the target. Dials out forever; never listens.
    Agent(AgentArgs),
}

#[derive(Args)]
struct AgentArgs {
    /// The controller's node id. Public, and the only thing baked into an image.
    #[arg(long)]
    controller: String,
    /// Where this agent's own keypair lives; generated on first run.
    #[arg(long, default_value = "/var/lib/egdod/agent.key")]
    key_file: PathBuf,
    /// Relay URL(s). Give an IP-literal URL when there is no DNS.
    #[arg(long = "relay")]
    relay: Vec<String>,
    /// No relay at all: only the addresses given with --direct.
    #[arg(long, conflicts_with = "relay")]
    no_relay: bool,
    /// Dial this socket address directly. Repeatable. Supplying it also switches
    /// off DNS-based address lookup, so the agent needs no resolver.
    #[arg(long = "direct")]
    direct: Vec<SocketAddr>,
}

#[derive(Subcommand)]
enum ControllerCmd {
    /// Creates the controller key and prints the node id.
    Init,
    /// Accepts agents and serves commands. Long-lived.
    Serve {
        #[arg(long = "relay")]
        relay: Vec<String>,
        #[arg(long, conflicts_with = "relay")]
        no_relay: bool,
        /// Seconds between reachability probes of our own node id.
        #[arg(long, default_value_t = 60)]
        probe_interval: u64,
        #[arg(long, default_value_t = 20)]
        probe_timeout: u64,
        /// Rebuild the iroh endpoint after this many consecutive failed probes.
        #[arg(long, default_value_t = 2)]
        restart_after: u32,
        /// Pin the UDP socket to this address, so a target can be given a fixed
        /// --direct address that survives a controller restart.
        #[arg(long)]
        bind: Option<SocketAddr>,
    },
    /// Agents that have dialled in and are waiting to be approved.
    Pending {
        #[arg(long)]
        json: bool,
    },
    /// Approve an agent by public key. Scriptable, and it survives a restart.
    Approve { pubkey: String },
    /// The controller's own view of itself, including whether it is dialable.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Run a command on the target and exit with its status.
    Exec {
        agent: String,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Copy a local file to the target.
    Push {
        agent: String,
        local: PathBuf,
        remote: String,
    },
    /// Copy a file from the target.
    Pull {
        agent: String,
        remote: String,
        local: PathBuf,
    },
    /// Tunnel a local TCP port to an address on the target.
    Forward {
        agent: String,
        local_port: u16,
        remote: String,
    },
    /// ssh to the target, composed from exec + copy + forward.
    Ssh {
        agent: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long, default_value = "/root/.ssh/authorized_keys")]
        authorized_keys: String,
        #[arg(long, default_value = "/etc/ssh/ssh_host_ed25519_key.pub")]
        host_key: String,
        /// argv used to start sshd on the target, repeat once per word.
        #[arg(long = "sshd-arg", allow_hyphen_values = true, default_values = ["sshd"])]
        sshd_arg: Vec<String>,
        /// argv used to generate host keys on the target, repeat once per word.
        #[arg(long = "keygen-arg", allow_hyphen_values = true, default_values = ["ssh-keygen", "-A"])]
        keygen_arg: Vec<String>,
        /// The target's sshd address, as seen from the target.
        #[arg(long, default_value = "127.0.0.1:22")]
        target_addr: String,
        /// Local port for the forward; 0 picks a free one.
        #[arg(long, default_value_t = 0)]
        local_port: u16,
        /// Everything after `--` is passed to ssh.
        #[arg(last = true)]
        ssh_argv: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "egdod=info,warn".into()),
        )
        .with_writer(std::io::stderr)
        // Logs are routinely captured to a file (a serial console, a demo
        // transcript); escape codes there are noise.
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    match Cli::parse().role {
        Role::Agent(a) => {
            let controller = parse_pubkey(&a.controller).context("--controller")?;
            let relay = RelayChoice::from_flags(a.no_relay, &a.relay);
            agent::run(agent::Config {
                controller,
                key_file: a.key_file,
                relay,
                direct: a.direct,
            })
            .await
        }
        Role::Controller { state_dir, cmd } => {
            let state = StateDir::new(state_dir.unwrap_or_else(StateDir::default_path));
            run_controller(state, cmd).await
        }
    }
}

async fn run_controller(state: StateDir, cmd: ControllerCmd) -> Result<()> {
    match cmd {
        ControllerCmd::Init => controller::init(&state),
        ControllerCmd::Serve {
            relay,
            no_relay,
            probe_interval,
            probe_timeout,
            restart_after,
            bind,
        } => {
            controller::serve(ServeConfig {
                state,
                relay: RelayChoice::from_flags(no_relay, &relay),
                probe_interval: Duration::from_secs(probe_interval),
                probe_timeout: Duration::from_secs(probe_timeout),
                restart_after,
                bind_addr: bind,
            })
            .await
        }
        ControllerCmd::Pending { json } => controller::pending(&state, json),
        ControllerCmd::Approve { pubkey } => controller::approve(&state, &pubkey),
        ControllerCmd::Status { json } => controller::status(&state, json),
        ControllerCmd::Exec { agent, argv } => {
            let code = controller::exec(&state, &agent, argv).await?;
            std::process::exit(code);
        }
        ControllerCmd::Push {
            agent,
            local,
            remote,
        } => controller::push(&state, &agent, &local, &remote).await,
        ControllerCmd::Pull {
            agent,
            remote,
            local,
        } => controller::pull(&state, &agent, &remote, &local).await,
        ControllerCmd::Forward {
            agent,
            local_port,
            remote,
        } => controller::forward(&state, &agent, local_port, remote).await,
        ControllerCmd::Ssh {
            agent,
            user,
            authorized_keys,
            host_key,
            sshd_arg,
            keygen_arg,
            target_addr,
            local_port,
            ssh_argv,
        } => {
            let cfg = ssh::Config {
                agent,
                user,
                authorized_keys,
                host_key_pub: host_key,
                sshd_argv: sshd_arg,
                keygen_argv: keygen_arg,
                target_addr,
                local_port,
                ssh_argv,
            };
            let code = ssh::run(&state, cfg).await?;
            std::process::exit(code);
        }
    }
}
