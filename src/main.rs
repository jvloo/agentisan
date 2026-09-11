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
    about = "Craft and inspect durable agent teams through a terminal dashboard, CLI, and MCP"
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
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the live, read-only terminal dashboard (also the default with no subcommand).
    Dashboard {
        /// Open with this run selected; defaults to the newest run.
        run_id: Option<String>,
        /// Open with this agent selected and its communication filtered.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Open one released agent session in an exact native client.
    Open {
        run_id: String,
        agent_id: String,
        #[arg(long, value_enum, default_value = "auto")]
        target: agentisan::dashboard::OpenTarget,
    },
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
    Mcp {
        /// Select the least-privilege tool surface for this host.
        #[arg(long, value_enum)]
        profile: mcp::Profile,
    },
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
    /// Trusted local administration of human decision records.
    Decisions {
        #[command(subcommand)]
        command: DecisionsCommand,
    },
    /// Trusted local inspection and bounded recovery of assignment records.
    Assignments {
        #[command(subcommand)]
        command: AssignmentsCommand,
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
    /// Reconcile one interrupted turn after inspecting its native evidence.
    Reconcile {
        turn_id: String,
        #[arg(long)]
        no_effect: bool,
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
enum DecisionsCommand {
    Inspect {
        decision_id: String,
    },
    Resolve {
        decision_id: String,
        #[arg(long)]
        scope_hash: String,
        #[arg(long)]
        artifact_hash: String,
        #[arg(long)]
        choice: String,
    },
    Invalidate {
        decision_id: String,
    },
}
#[derive(Subcommand)]
enum AssignmentsCommand {
    Inspect {
        assignment_id: String,
    },
    /// Add capacity from the run's existing root limits after inspecting a stalled assignment.
    Extend {
        assignment_id: String,
        #[arg(long, default_value_t = 0)]
        add_turns: u32,
        #[arg(long, default_value_t = 0)]
        add_messages: u32,
        /// Extend from now, capped by the original run deadline; zero leaves it unchanged.
        #[arg(long, default_value_t = 0)]
        deadline_seconds: u32,
        #[arg(long)]
        after_inspection: bool,
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
    let mut cli = Cli::parse();
    let command = cli.command.take().unwrap_or(Command::Dashboard {
        run_id: None,
        agent: None,
    });
    // The process-group anchor only uses std process/thread APIs. Avoid creating
    // a Tokio worker pool for every supervised CLI invocation.
    if let Command::WorkerHost {
        exit_file,
        timeout_ms,
        command,
    } = &command
    {
        return agentisan::worker::host(exit_file, *timeout_ms, command);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli, command))
}

async fn run(cli: Cli, command: Command) -> Result<()> {
    let query = match command {
        Command::Dashboard { run_id, agent } => {
            return agentisan::dashboard::run(&cli.data_dir, run_id.as_deref(), agent.as_deref())
                .await;
        }
        Command::Open {
            run_id,
            agent_id,
            target,
        } => {
            return agentisan::dashboard::open_native(&cli.data_dir, &run_id, &agent_id, target)
                .await;
        }
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
        Command::Mcp { profile } => {
            return mcp::run(
                Client::new(&cli.endpoint, cli.credential_file.as_deref())?,
                profile,
            )
            .await;
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
        Command::Runs {
            command:
                RunsCommand::Reconcile {
                    turn_id,
                    no_effect,
                    after_inspection,
                },
        } => {
            if !no_effect || !after_inspection {
                bail!("reconciliation requires --no-effect and --after-inspection");
            }
            let _lock = fixture::admin_lock(&cli.data_dir)?;
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            let value = teams::reconcile_no_effect(&registry, &turn_id).await?;
            registry.close().await;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        Command::Messages {
            command: MessagesCommand::List { run },
        } => Query::MessagesList { run_id: run },
        Command::Decisions {
            command: DecisionsCommand::Inspect { decision_id },
        } => {
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            let value = agentisan::assignments::inspect_decision(&registry, &decision_id).await?;
            registry.close().await;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        Command::Decisions {
            command:
                DecisionsCommand::Resolve {
                    decision_id,
                    scope_hash,
                    artifact_hash,
                    choice,
                },
        } => {
            let _lock = fixture::admin_lock(&cli.data_dir)?;
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            let value = agentisan::assignments::resolve_decision(
                &registry,
                &decision_id,
                &scope_hash,
                &artifact_hash,
                &choice,
                "local_os_admin",
            )
            .await?;
            registry.close().await;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        Command::Decisions {
            command: DecisionsCommand::Invalidate { decision_id },
        } => {
            let _lock = fixture::admin_lock(&cli.data_dir)?;
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            agentisan::assignments::invalidate_decision(&registry, &decision_id).await?;
            registry.close().await;
            println!(
                "{}",
                serde_json::json!({"decision_id":decision_id,"state":"invalidated"})
            );
            return Ok(());
        }
        Command::Assignments {
            command: AssignmentsCommand::Inspect { assignment_id },
        } => {
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            let value =
                agentisan::assignments::inspect_assignment(&registry, &assignment_id).await?;
            registry.close().await;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        Command::Assignments {
            command:
                AssignmentsCommand::Extend {
                    assignment_id,
                    add_turns,
                    add_messages,
                    deadline_seconds,
                    after_inspection,
                },
        } => {
            if !after_inspection {
                bail!("inspect the assignment first, then acknowledge with --after-inspection");
            }
            let _lock = fixture::admin_lock(&cli.data_dir)?;
            let registry = Registry::open(&fixture::database_path(&cli.data_dir)?).await?;
            let value = agentisan::assignments::extend_assignment(
                &registry,
                &assignment_id,
                add_turns,
                add_messages,
                deadline_seconds,
            )
            .await?;
            registry.close().await;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
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
