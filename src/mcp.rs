//! Role-scoped stdio MCP connectors. Only the separate service owns durable state.
use crate::{
    client::Client,
    model::{AgentId, GroupId, Query, TeamId},
};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};

const MAX_MCP_RESULT_BYTES: usize = 131_072;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Profile {
    /// A narrow, credential-bound surface used by a managed agent turn.
    Agent,
    /// A bounded, read-only inspection surface for trusted clients.
    Observer,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyArgs {}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupArgs {
    pub group_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TeamArgs {
    pub team_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentArgs {
    pub agent_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunArgs {
    /// Exact run assigned by the host. The service validates it against this credential.
    pub run_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendArgs {
    /// Exact run assigned by the host. The service validates it against this credential.
    pub run_id: String,
    /// Exact canonical teammate ID. The sender is always derived from the credential.
    pub to: String,
    /// Message text, limited to 8 KiB.
    pub body: String,
    /// Original message ID when replying.
    pub reply_to: Option<String>,
    /// Stable key for this logical send. Reuse only for an identical request.
    pub idempotency_key: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeArgs {
    /// Exact run assigned by the host. The service validates it against this credential.
    pub run_id: String,
    /// Proposed result, limited to 16 KiB. Proposal is not acceptance.
    pub result: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitArgs {
    /// Exact run assigned by the host. The lease binds it to the active turn.
    pub run_id: String,
    /// Stable key for this logical commit. An exact retry returns the first receipt.
    pub idempotency_key: String,
}

fn success(value: serde_json::Value) -> CallToolResult {
    let encoded = value.to_string();
    if encoded.len() > MAX_MCP_RESULT_BYTES {
        return CallToolResult::error(vec![ContentBlock::text(
            "response exceeds the MCP output limit",
        )]);
    }
    CallToolResult::success(vec![ContentBlock::text(encoded)])
}

fn rejected_code(code: &'static str) -> CallToolResult {
    // Service errors can contain database, transport, or policy details. Keep the
    // public MCP boundary closed; authoritative diagnostics stay in service logs.
    CallToolResult::error(vec![ContentBlock::text(
        serde_json::json!({"error":{"code":code}}).to_string(),
    )])
}

fn rejected() -> CallToolResult {
    rejected_code("request_rejected")
}

fn action_rejected(error: &anyhow::Error) -> CallToolResult {
    let detail = error.to_string();
    let code = if detail.contains("lease_not_active") {
        "lease_not_active"
    } else if detail.contains("action_not_permitted") {
        "role_forbidden"
    } else if detail.contains("work_pending") {
        "work_pending"
    } else if detail.contains("budget_exhausted") {
        "budget_exhausted"
    } else if detail.contains("idempotency_conflict") {
        "idempotency_conflict"
    } else if detail.contains("invalid_reply") {
        "invalid_reply"
    } else if detail.contains("only the lead may propose completion") {
        "role_forbidden"
    } else if detail.contains("messages are still pending")
        || detail.contains("teammates must settle")
    {
        "work_pending"
    } else if detail.contains("message budget exhausted") {
        "budget_exhausted"
    } else if detail.contains("run is not accepting agent actions") {
        "run_not_active"
    } else if detail.contains("no owned active turn") {
        "turn_not_owned"
    } else if detail.contains("exact current turn lease")
        || detail.contains("turn lease is fenced")
        || detail.contains("turn lease is already committed")
    {
        "lease_not_active"
    } else if detail.contains("invalid commit idempotency") {
        "invalid_commit"
    } else if detail.contains("recipient not in this team") {
        "recipient_not_available"
    } else if detail.contains("idempotency key reused") {
        "idempotency_conflict"
    } else if detail.contains("reply must address the original sender") {
        "invalid_reply"
    } else {
        "request_rejected"
    };
    rejected_code(code)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

#[derive(Clone)]
pub struct ObserverServer {
    client: Client,
}

impl ObserverServer {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    async fn query(&self, query: Query) -> Result<CallToolResult, ErrorData> {
        Ok(match self.client.inspect(query).await {
            Ok(value) => success(value),
            Err(_) => rejected(),
        })
    }
}

#[tool_router]
impl ObserverServer {
    #[tool(
        description = "Read this connector's explicitly provisioned identity. Never starts or resumes execution.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn whoami(&self) -> Result<CallToolResult, ErrorData> {
        self.query(Query::Whoami {}).await
    }

    #[tool(
        description = "List authorized groups. Read-only and bounded by the MCP response limit.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn groups_list(&self) -> Result<CallToolResult, ErrorData> {
        self.query(Query::GroupsList {}).await
    }

    #[tool(
        description = "List teams in an authorized group. Never creates or runs agents.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn teams_list(
        &self,
        Parameters(args): Parameters<GroupArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.group_id) {
            return Ok(rejected());
        }
        self.query(Query::TeamsList {
            group_id: GroupId(args.group_id),
        })
        .await
    }

    #[tool(
        description = "List agents in an authorized team. Native IDs do not prove liveness or authority.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn agents_list(
        &self,
        Parameters(args): Parameters<TeamArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.team_id) {
            return Ok(rejected());
        }
        self.query(Query::AgentsList {
            team_id: TeamId(args.team_id),
        })
        .await
    }

    #[tool(
        description = "Inspect one exact authorized agent. Never starts or resumes execution.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn agents_inspect(
        &self,
        Parameters(args): Parameters<AgentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.agent_id) {
            return Ok(rejected());
        }
        self.query(Query::AgentsInspect {
            agent_id: AgentId(args.agent_id),
        })
        .await
    }

    #[tool(
        description = "Read authoritative state, limits and completed turn outputs for an authorized run.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn runs_inspect(
        &self,
        Parameters(args): Parameters<RunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.run_id) {
            return Ok(rejected());
        }
        self.query(Query::RunsInspect {
            run_id: args.run_id,
        })
        .await
    }

    #[tool(
        description = "Read ordered authorized run messages. Does not deliver or acknowledge them.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn messages_list(
        &self,
        Parameters(args): Parameters<RunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.run_id) {
            return Ok(rejected());
        }
        self.query(Query::MessagesList {
            run_id: args.run_id,
        })
        .await
    }
}

#[tool_handler]
impl ServerHandler for ObserverServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Agentisan observer profile. All tools are credential-scoped, bounded and read-only. Native identifiers do not grant authority. These tools never resume agents, send messages, approve decisions, or execute commands.")
    }
}

#[derive(Clone)]
pub struct AgentServer {
    client: Client,
    can_propose: bool,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

#[tool_router(router = tool_router)]
impl AgentServer {
    pub fn new(client: Client, can_propose: bool) -> Self {
        let mut tool_router = Self::tool_router();
        if !can_propose {
            tool_router.disable_route("result_propose");
        }
        Self {
            client,
            can_propose,
            tool_router,
        }
    }

    async fn query(&self, query: Query) -> Result<serde_json::Value, ()> {
        self.client.inspect(query).await.map_err(|_| ())
    }

    async fn action(&self, action: crate::teams::Action) -> Result<CallToolResult, ErrorData> {
        Ok(match self.client.act(action).await {
            Ok(value) => success(value),
            Err(error) => action_rejected(&error),
        })
    }
    #[tool(
        description = "Get your credential-bound Agentisan identity and protocol capabilities. No caller-supplied identity is accepted.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn agent_context_get(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let identity = match self.query(Query::Whoami {}).await {
            Ok(value) if value["status"] == "bound" => value,
            _ => return Ok(rejected()),
        };
        let mut capabilities = vec!["inbox_read", "message_send", "turn_commit"];
        if self.can_propose {
            capabilities.push("result_propose");
        }
        Ok(success(serde_json::json!({
            "protocol": "agentisan-agent-v1",
            "identity": identity,
            "role": if self.can_propose {"lead"} else {"worker"},
            "capabilities": capabilities,
            "run_binding": "validated_by_service_on_each_action",
            "delivery": "read_claims_input_without_ack; turn_commit_acknowledges_and_publishes_atomically"
        })))
    }

    #[tool(
        description = "Read the messages claimed for your active turn without acknowledging processing. Repeated reads return the same snapshot. Do not poll.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn inbox_read(
        &self,
        Parameters(args): Parameters<RunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.run_id) {
            return Ok(rejected());
        }
        self.action(crate::teams::Action::Receive {
            run_id: args.run_id,
        })
        .await
    }

    #[tool(
        description = "Atomically acknowledge this turn's claimed inputs and publish its staged messages. An exact retry returns the original receipt.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn turn_commit(
        &self,
        Parameters(args): Parameters<CommitArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.run_id) || !valid_identifier(&args.idempotency_key) {
            return Ok(rejected());
        }
        self.action(crate::teams::Action::Commit {
            run_id: args.run_id,
            idempotency_key: args.idempotency_key,
        })
        .await
    }

    #[tool(
        description = "Send a bounded message to an exact teammate. Sender identity comes from your credential. Reuse an idempotency key only for an identical send.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn message_send(
        &self,
        Parameters(args): Parameters<SendArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.run_id)
            || !valid_identifier(&args.to)
            || args.body.trim().is_empty()
            || args.body.len() > 8_192
            || !valid_identifier(&args.idempotency_key)
            || args
                .reply_to
                .as_deref()
                .is_some_and(|v| !valid_identifier(v))
        {
            return Ok(rejected());
        }
        self.action(crate::teams::Action::Send {
            run_id: args.run_id,
            to: AgentId(args.to),
            body: args.body,
            reply_to: args.reply_to,
            idempotency_key: args.idempotency_key,
        })
        .await
    }

    #[tool(
        description = "Lead only: propose a bounded final result after team work settles. This is neither independent acceptance nor human approval.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn result_propose(
        &self,
        Parameters(args): Parameters<ProposeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !valid_identifier(&args.run_id)
            || args.result.trim().is_empty()
            || args.result.len() > 16_384
        {
            return Ok(rejected());
        }
        self.action(crate::teams::Action::Complete {
            run_id: args.run_id,
            result: args.result,
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgentServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Agentisan agent profile. Identity and sender come from the per-turn lease credential; every action is restricted to its assigned active run and ownership epoch. Read the inbox once, do available work, stage bounded messages, then call turn_commit before ending the native turn. Only the lead may call result_propose; a successful proposal commits that lead turn automatically. Proposal is separate from acceptance. This profile cannot inspect groups, enumerate teams, read the full timeline, alter permissions, approve decisions, or execute commands.")
    }
}

pub async fn run(client: Client, profile: Profile) -> anyhow::Result<()> {
    match profile {
        Profile::Agent => {
            let identity = client.inspect(Query::Whoami {}).await?;
            let agent_id = identity["agent_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("agent MCP profile requires a bound credential"))?;
            let record = client
                .inspect(Query::AgentsInspect {
                    agent_id: AgentId(agent_id.to_string()),
                })
                .await?;
            let can_propose = record["agent"]["role"] == "lead";
            AgentServer::new(client, can_propose)
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
        }
        Profile::Observer => {
            ObserverServer::new(client)
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_proposal_is_only_discovered_by_leads() {
        let client = Client::new("http://127.0.0.1:7437", None).unwrap();
        let worker = AgentServer::new(client.clone(), false);
        let lead = AgentServer::new(client, true);
        assert!(!worker.tool_router.has_route("result_propose"));
        assert!(lead.tool_router.has_route("result_propose"));
        assert_eq!(worker.tool_router.list_all().len(), 4);
        assert_eq!(lead.tool_router.list_all().len(), 5);
    }
}
