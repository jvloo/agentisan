use agentisan::{
    client::Client,
    fixture, mcp,
    model::{AgentId, GroupId, Query, TeamId},
    registry::Registry,
    server, teams,
};
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "Run and inspect persistent agent teams through CLI and MCP"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".agentisan")]
    data_dir: PathBuf,
    #[arg(long, global = true, default_value = "http://127.0.0.1:7437")]
    endpoint: String,
    /// File containing an explicitly provisioned Agentisan credential. Never a native session ID.
    #[arg(long, global = true)]
    credential_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    WorkerHost {
        #[arg(long)]
        exit_file: PathBuf,
        #[arg(long)]
        timeout_ms: u64,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Local administration: import an immutable fake-adapter fixture and provision credentials.
    Init {
        #[arg(long)]
        fixture: PathBuf,
    },
    /// Run the separate inspection service. Only numeric loopback addresses are supported.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7437")]
        listen: SocketAddr,
        /// Explicitly allow execution of configured Claude/Codex CLI turns (macOS/Linux).
        #[arg(long)]
        enable_cli_workers: bool,
    },
    /// Connect an MCP host over stdio. Registry records remain in the separate service.
    Mcp,
    /// Report the credential's agent binding, or unbound when none is provided.
    Whoami,
    Groups {
        #[command(subcommand)]
        command: GroupsCommand,
    },
    Teams {
        #[command(subcommand)]
        command: TeamsCommand,
    },
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
    Messages {
        #[command(subcommand)]
        command: MessagesCommand,
    },
}
#[derive(Subcommand)]
enum GroupsCommand {
    List,
}
#[derive(Subcommand)]
enum TeamsCommand {
    List {
        #[arg(long)]
        group: String,
    },
    /// Register a reusable managed team from an explicit local configuration.
    Create {
        #[arg(long)]
        config: PathBuf,
    },
    /// Submit a real objective to the independently running worker service.
    Run {
        #[arg(long)]
        team: String,
        #[arg(long)]
        prompt_file: PathBuf,
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 18)]
        max_turns: u32,
        #[arg(long, default_value_t = 64)]
        max_messages: u32,
        #[arg(long, default_value_t = 900)]
        timeout_seconds: u64,
        #[arg(long, default_value_t = 120)]
        turn_timeout_seconds: u64,
    },
}
#[derive(Subcommand)]
enum RunsCommand {
    Inspect {
        run_id: String,
    },
    /// Poll authoritative Agentisan state and print changed snapshots as JSON lines.
    Watch {
        run_id: String,
        #[arg(long, default_value_t = 500)]
        interval_ms: u64,
        #[arg(long, default_value_t = 300)]
        timeout_seconds: u64,
    },
    /// Run one administrator-selected deterministic verifier against a completed proposal.
    Verify {
        run_id: String,
        #[arg(long)]
        verifier: PathBuf,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
    },
    /// Resume unread work after inspecting a stalled run; original limits stay in force.
    Resume {
        run_id: String,
        #[arg(long)]
        after_inspection: bool,
    },
}
#[derive(Subcommand)]
enum MessagesCommand {
    List {
        #[arg(long)]
        run: String,
    },
}
#[derive(Subcommand)]
enum AgentsCommand {
    List {
        #[arg(long)]
        team: String,
    },
    Inspect {
        agent_id: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // The process-group anchor only uses std process/thread APIs. Avoid creating
    // a Tokio worker pool for every supervised CLI invocation.
    if let Command::WorkerHost {
        exit_file,
        timeout_ms,
        command,
    } = &cli.command
    {
        return agentisan::worker::host(exit_file, *timeout_ms, command);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let query = match cli.command {
        Command::WorkerHost {
            exit_file,
            timeout_ms,
            command,
        } => {
            return agentisan::worker::host(&exit_file, timeout_ms, &command);
        }
        Command::Init { fixture: path } => {
            let credentials = fixture::initialize(&cli.data_dir, &path).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"source":"fixture","credential_files":credentials})
                )?
            );
            return Ok(());
        }
        Command::Serve {
            listen,
            enable_cli_workers,
        } => {
            if !listen.ip().is_loopback() {
                bail!("only loopback listeners are supported");
            }
            let path = fixture::database_path(&cli.data_dir)?;
            if !path.is_file() {
                bail!("registry not initialized; use agentisan init --fixture <file>");
            }
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(cli.data_dir.join("service.lock"))?;
            lock.try_lock().map_err(|_| {
                anyhow::anyhow!("another Agentisan service owns this data directory")
            })?;
            let registry = Registry::open(&path).await?;
            if !registry.is_initialized().await? {
                bail!(
                    "registry initialization is incomplete; import a valid fixture or create a managed team"
                );
            }
            let listener = tokio::net::TcpListener::bind(listen).await?;
            // A single machine-readable readiness line; no credentials or records.
            println!(
                "{}",
                serde_json::json!({"service":"agentisan","address":listener.local_addr()?.to_string()})
            );
            let result = server::serve(
                registry,
                listener,
                if enable_cli_workers {
                    Some(cli.data_dir.canonicalize()?)
                } else {
                    None
                },
            )
            .await;
            drop(lock);
            return result;
        }
        Command::Mcp => {
            return mcp::run(Client::new(&cli.endpoint, cli.credential_file.as_deref())?).await;
        }
        Command::Whoami => Query::Whoami {},
        Command::Groups {
            command: GroupsCommand::List,
        } => Query::GroupsList {},
        Command::Teams {
            command: TeamsCommand::List { group },
        } => Query::TeamsList {
            group_id: GroupId(group),
        },
        Command::Teams {
            command: TeamsCommand::Create { config },
        } => {
            let text = std::fs::read_to_string(config)?;
            if text.len() > 131072 {
                bail!("team configuration too large");
            }
            let config: teams::TeamConfig = serde_json::from_str(&text)?;
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            let out = teams::create(&registry, &cli.data_dir.canonicalize()?, &config).await?;
            registry.close().await;
            println!("{}", serde_json::to_string_pretty(&out)?);
            return Ok(());
        }
        Command::Teams {
            command:
                TeamsCommand::Run {
                    team,
                    prompt_file,
                    live,
                    max_turns,
                    max_messages,
                    timeout_seconds,
                    turn_timeout_seconds,
                },
        } => {
            if !live {
                bail!("teams run requires --live to authorize actual provider calls");
            }
            let objective = std::fs::read_to_string(prompt_file)?;
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            let id = teams::start(
                &registry,
                &team,
                &objective,
                max_turns,
                max_messages,
                timeout_seconds,
                turn_timeout_seconds,
            )
            .await?;
            registry.close().await;
            println!("{}", serde_json::json!({"run_id":id,"state":"queued"}));
            return Ok(());
        }
        Command::Runs {
            command: RunsCommand::Inspect { run_id },
        } => Query::RunsInspect { run_id },
        Command::Runs {
            command:
                RunsCommand::Watch {
                    run_id,
                    interval_ms,
                    timeout_seconds,
                },
        } => {
            if !(100..=60_000).contains(&interval_ms) || !(1..=3600).contains(&timeout_seconds) {
                bail!("watch interval or deadline is outside the bounded range");
            }
            let client = Client::new(&cli.endpoint, cli.credential_file.as_deref())?;
            let started = std::time::Instant::now();
            let mut previous = None;
            loop {
                let value = client
                    .inspect(Query::RunsInspect {
                        run_id: run_id.clone(),
                    })
                    .await?;
                let encoded = serde_json::to_string(&value)?;
                if previous.as_deref() != Some(encoded.as_str()) {
                    println!("{encoded}");
                    previous = Some(encoded);
                }
                let terminal = matches!(
                    value["state"].as_str(),
                    Some("completed" | "failed" | "exhausted" | "stalled" | "interrupted")
                );
                if terminal {
                    return Ok(());
                }
                if started.elapsed() >= std::time::Duration::from_secs(timeout_seconds) {
                    bail!("watch deadline reached before a terminal run state");
                }
                tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
            }
        }
        Command::Runs {
            command:
                RunsCommand::Verify {
                    run_id,
                    verifier,
                    timeout_seconds,
                },
        } => {
            let path = fixture::database_path(&cli.data_dir)?;
            if !path.is_file() {
                bail!("registry not initialized");
            }
            let registry = Registry::open(&path).await?;
            let value = agentisan::verification::verify(
                &registry,
                &cli.data_dir.canonicalize()?,
                &run_id,
                &verifier,
                timeout_seconds,
            )
            .await?;
            registry.close().await;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        Command::Runs {
            command:
                RunsCommand::Resume {
                    run_id,
                    after_inspection,
                },
        } => {
            if !after_inspection {
                bail!("inspect the run first, then acknowledge with --after-inspection");
            }
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            teams::resume(&registry, &run_id).await?;
            registry.close().await;
            println!(
                "{}",
                serde_json::json!({"run_id":run_id,"state":"running","limits":"unchanged"})
            );
            return Ok(());
        }
        Command::Messages {
            command: MessagesCommand::List { run },
        } => Query::MessagesList { run_id: run },
        Command::Agents {
            command: AgentsCommand::List { team },
        } => Query::AgentsList {
            team_id: TeamId(team),
        },
        Command::Agents {
            command: AgentsCommand::Inspect { agent_id },
        } => Query::AgentsInspect {
            agent_id: AgentId(agent_id),
        },
    };
    let value = Client::new(&cli.endpoint, cli.credential_file.as_deref())?
        .inspect(query)
        .await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
