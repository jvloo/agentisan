use agentisan::{
    client::Client,
    fixture, mcp,
    model::{AgentId, GroupId, Query, TeamId},
    registry::Registry,
    server,
};
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "Inspect persistent simulated agent teams through CLI and MCP"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".agentisan")]
    data_dir: PathBuf,
    #[arg(long, global = true, default_value = "http://127.0.0.1:7437")]
    endpoint: String,
    /// File containing an explicitly provisioned fixture credential. Never a native session ID.
    #[arg(long, global = true)]
    credential_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Local administration: import an immutable fake-adapter fixture and provision credentials.
    Init {
        #[arg(long)]
        fixture: PathBuf,
    },
    /// Run the separate inspection service. Only numeric loopback addresses are supported.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7437")]
        listen: SocketAddr,
    },
    /// Connect an MCP host over stdio. Registry records remain in the separate service.
    Mcp,
    /// Report the credential's fixture binding, or unbound when none is provided.
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let query = match cli.command {
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
        Command::Serve { listen } => {
            if !listen.ip().is_loopback() {
                bail!("only loopback listeners are supported");
            }
            let path = fixture::database_path(&cli.data_dir)?;
            if !path.is_file() {
                bail!("registry not initialized; use agentisan init --fixture <file>");
            }
            let registry = Registry::open(&path).await?;
            let listener = tokio::net::TcpListener::bind(listen).await?;
            // A single machine-readable readiness line; no credentials or records.
            println!(
                "{}",
                serde_json::json!({"service":"agentisan","address":listener.local_addr()?.to_string()})
            );
            return server::serve(registry, listener).await;
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
