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
    /// A managed lead credential that can start and inspect its team.
    Controller,
    /// An interactive host chat that coordinates configured workers directly.
    Operator,
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
    } else if detail.contains("recipient not in this team")
        || detail.contains("recipient not engaged")
    {
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

fn start_rejected(error: &anyhow::Error) -> CallToolResult {
    let detail = error.to_string();
    let code = if detail.contains("scheduler_unavailable") {
        "scheduler_unavailable"
    } else if detail.contains("controller_forbidden") {
        "controller_forbidden"
    } else if detail.contains("idempotency_conflict") {
        "idempotency_conflict"
    } else if detail.contains("run_already_active") {
        "run_already_active"
    } else if detail.contains("live_authorization_required") {
        "live_authorization_required"
    } else if detail.contains("invalid_request") {
        "invalid_request"
    } else {
        "request_rejected"
    };
    rejected_code(code)
}

fn interactive_rejected(error: &anyhow::Error) -> CallToolResult {
    let detail = error.to_string();
    let code = [
        "scheduler_unavailable",
        "controller_forbidden",
        "invalid_control_handle",
        "connector_fenced",
        "stale_epoch",
        "stale_version",
        "idempotency_conflict",
        "run_already_active",
        "live_authorization_required",
        "work_pending",
        "run_already_settled",
    ]
    .into_iter()
    .find(|code| detail.contains(code))
    .unwrap_or("request_rejected");
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
pub struct ControllerServer {
    client: Client,
    agent_id: String,
    team_id: String,
}

impl ControllerServer {
    pub fn new(client: Client, agent_id: String, team_id: String) -> Self {
        Self {
            client,
            agent_id,
            team_id,
        }
    }

    async fn scoped_run(&self, run_id: &str) -> Result<serde_json::Value, ()> {
        let value = self
            .client
            .inspect(Query::RunsInspect {
                run_id: run_id.to_string(),
            })
            .await
            .map_err(|_| ())?;
        if value["team_id"] != self.team_id {
            return Err(());
        }
        Ok(value)
    }
}

#[tool_router]
impl ControllerServer {
    #[tool(
        description = "Get this controller's credential-bound managed team. The MCP host chat is a controller and is not asserted as the team lead session.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn controller_context_get(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let identity = match self.client.inspect(Query::Whoami {}).await {
            Ok(value) => value,
            Err(_) => return Ok(rejected()),
        };
        Ok(success(serde_json::json!({
            "protocol": "agentisan-controller-v1",
            "identity": identity,
            "agent_id": self.agent_id,
            "team_id": self.team_id,
            "controller_identity": "managed_team_lead_credential",
            "chat_identity": "not_asserted",
            "capabilities": ["team_run_start","team_members_list","runs_inspect","messages_list"]
        })))
    }

    #[tool(
        description = "List the members of this controller's managed team. Never starts or resumes execution.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn team_members_list(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(
            match self
                .client
                .inspect(Query::AgentsList {
                    team_id: TeamId(self.team_id.clone()),
                })
                .await
            {
                Ok(value) => success(value),
                Err(_) => rejected(),
            },
        )
    }

    #[tool(
        description = "Start this controller's managed team with bounded limits. Requires live=true because it launches actual configured provider calls. Use one stable idempotency key for one logical start.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn team_run_start(
        &self,
        Parameters(args): Parameters<crate::teams::ControllerStartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(match self.client.start_run(args).await {
            Ok(value) => success(value),
            Err(error) => start_rejected(&error),
        })
    }

    #[tool(
        description = "Inspect authoritative state, limits and completed turn outputs for one run of this managed team.",
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
        Ok(match self.scoped_run(&args.run_id).await {
            Ok(value) => success(value),
            Err(_) => rejected(),
        })
    }

    #[tool(
        description = "Read the ordered messages for one run of this managed team. Does not deliver or acknowledge them.",
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
        if !valid_identifier(&args.run_id) || self.scoped_run(&args.run_id).await.is_err() {
            return Ok(rejected());
        }
        Ok(
            match self
                .client
                .inspect(Query::MessagesList {
                    run_id: args.run_id,
                })
                .await
            {
                Ok(value) => success(value),
                Err(_) => rejected(),
            },
        )
    }
}

#[tool_handler]
impl ServerHandler for ControllerServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Agentisan controller profile for a planning chat. The credential selects one managed team; the chat itself is not asserted as the native team lead. Call team_run_start only after the user asks to launch the team and set live=true only with that authorization. Reuse the same idempotency key for an exact retry. Use the returned run_id with runs_inspect and messages_list. Agentisan owns team execution, peer communication, limits, and receipts.")
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperatorStartArgs {
    pub objective: String,
    pub live: bool,
    pub workers: Vec<String>,
    pub initial_work: Vec<crate::interactive::InitialWorkItem>,
    pub idempotency_key: String,
    pub max_turns: Option<u32>,
    pub max_messages: Option<u32>,
    pub timeout_seconds: Option<u64>,
    pub turn_timeout_seconds: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperatorUpdateArgs {
    pub run_id: String,
    pub control_handle: String,
    pub expected_version: i64,
    pub expected_epoch: i64,
    pub idempotency_key: String,
    pub action: crate::interactive::UpdateAction,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperatorCancelArgs {
    pub run_id: String,
    pub control_handle: String,
    pub expected_version: i64,
    pub expected_epoch: i64,
    pub idempotency_key: String,
    pub reason: Option<String>,
}

#[derive(Clone)]
pub struct OperatorServer {
    client: Client,
    connector_instance: String,
}

impl OperatorServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            connector_instance: format!("connector_{}", uuid::Uuid::new_v4().simple()),
        }
    }
}

#[tool_router]
impl OperatorServer {
    #[tool(
        description = "Start an interactive run that sends bounded work directly to exact configured workers. No managed model lead is launched. Requires live=true for actual provider calls.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn team_start(
        &self,
        Parameters(args): Parameters<OperatorStartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let request = crate::interactive::StartArgs {
            objective: args.objective,
            live: args.live,
            connector_instance: self.connector_instance.clone(),
            workers: args.workers,
            initial_work: args.initial_work,
            idempotency_key: args.idempotency_key,
            max_turns: args.max_turns,
            max_messages: args.max_messages,
            timeout_seconds: args.timeout_seconds,
            turn_timeout_seconds: args.turn_timeout_seconds,
        };
        Ok(match self.client.start_interactive(request).await {
            Ok(value) => success(value),
            Err(error) => interactive_rejected(&error),
        })
    }

    #[tool(
        description = "Read interactive run state, worker turns, committed peer-message previews and latest controller reports. timeout_seconds=0 snapshots immediately; message_id fetches one exact committed body; a bounded wait never renews control or starts model work.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn team_status(
        &self,
        Parameters(args): Parameters<crate::interactive::StatusArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(match self.client.interactive_status(args).await {
            Ok(value) => success(value),
            Err(error) => interactive_rejected(&error),
        })
    }

    #[tool(
        description = "Update an interactive run within its existing envelope: message a worker, accept a committed worker report, or finish a settled run. Requires the per-run control handle and optimistic version/epoch.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn team_update(
        &self,
        Parameters(args): Parameters<OperatorUpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let request = crate::interactive::UpdateArgs {
            run_id: args.run_id,
            control_handle: args.control_handle,
            connector_instance: self.connector_instance.clone(),
            expected_version: args.expected_version,
            expected_epoch: args.expected_epoch,
            idempotency_key: args.idempotency_key,
            action: args.action,
        };
        Ok(match self.client.interactive_update(request).await {
            Ok(value) => success(value),
            Err(error) => interactive_rejected(&error),
        })
    }

    #[tool(
        description = "Request cancellation of an interactive run. With no active native turn the stop is confirmed; otherwise the result remains explicitly unconfirmed for reconciliation.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn team_cancel(
        &self,
        Parameters(args): Parameters<OperatorCancelArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let request = crate::interactive::CancelArgs {
            run_id: args.run_id,
            control_handle: args.control_handle,
            connector_instance: self.connector_instance.clone(),
            expected_version: args.expected_version,
            expected_epoch: args.expected_epoch,
            idempotency_key: args.idempotency_key,
            reason: args.reason,
        };
        Ok(match self.client.interactive_cancel(request).await {
            Ok(value) => success(value),
            Err(error) => interactive_rejected(&error),
        })
    }
}

#[tool_handler]
impl ServerHandler for OperatorServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Agentisan interactive operator profile. This MCP host chat coordinates configured workers directly; Agentisan does not launch the configured model lead. Start only after the user asks to use an agent team and set live=true only within that authorization. Keep the returned control_handle private to this chat context. Use team_status with a bounded wait instead of polling. Accept committed reports before finishing. This profile cannot resolve trusted human decisions or expand the root budget.")
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
            tool_router.disable_route("assignment_create");
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
        let mut capabilities = vec![
            "inbox_read",
            "message_send",
            "turn_commit",
            "assignment_update",
            "decision_request",
        ];
        if self.can_propose {
            capabilities.push("result_propose");
            capabilities.push("assignment_create");
        }
        let run = if let Some(run_id) = identity["lease"]["run_id"].as_str() {
            self.query(Query::RunsInspect {
                run_id: run_id.to_string(),
            })
            .await
            .ok()
        } else {
            None
        };
        let coordination_mode = run
            .as_ref()
            .and_then(|value| value["mode"].as_str())
            .unwrap_or("unknown");
        let controller_mailbox = if coordination_mode == "interactive" {
            run.as_ref().and_then(|value| value["lead_id"].as_str())
        } else {
            None
        };
        Ok(success(serde_json::json!({
            "protocol": "agentisan-agent-v1",
            "identity": identity,
            "role": if self.can_propose {"lead"} else {"worker"},
            "capabilities": capabilities,
            "coordination_mode": coordination_mode,
            "controller_mailbox": controller_mailbox,
            "run_binding": "validated_by_service_on_each_action",
            "delivery": "read_claims_input_without_ack; turn_commit_acknowledges_and_publishes_atomically"
        })))
    }

    #[tool(
        description = "Read the messages claimed for your active turn and record that the snapshot was presented, without acknowledging processing. Repeated reads return the same snapshot. Do not poll.",
        annotations(
            read_only_hint = false,
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

    #[tool(
        description = "Lead only: stage a bounded assignment to an exact teammate. Agentisan derives its immutable scope hash and returns it. deadline_seconds must be 30..3600; budgets reserve a slice of the remaining root limits. Publish with turn_commit.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn assignment_create(
        &self,
        Parameters(args): Parameters<crate::assignments::CreateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.action(crate::teams::Action::AssignmentCreate(args))
            .await
    }

    #[tool(
        description = "Stage an assignment transition. Assignees can accept, start or report; only the lead closes or cancels. Published atomically by turn_commit.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn assignment_update(
        &self,
        Parameters(args): Parameters<crate::assignments::UpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.action(crate::teams::Action::AssignmentUpdate(args))
            .await
    }

    #[tool(
        description = "Stage a human decision request for an immutable scope and artifact revision. This never grants approval. Publish with turn_commit; only a trusted administrator can resolve it.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn decision_request(
        &self,
        Parameters(args): Parameters<crate::assignments::DecisionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.action(crate::teams::Action::DecisionRequest(args))
            .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgentServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Agentisan agent profile. Identity and sender come from the per-turn lease credential; every action is restricted to its assigned active run and ownership epoch. Read the inbox once and inspect coordination_mode. In interactive mode, no native model lead will run: exchange bounded peer messages as assigned and send the final report to controller_mailbox, then commit. In autonomous mode, leads create bounded assignments; assignees report them and leads close them. A decision_request asks a human but never grants approval. Stage messages and work updates, then call turn_commit before ending the native turn. Only a native lead may call result_propose; proposal is separate from acceptance. This profile cannot inspect groups, enumerate teams, read the full timeline, alter permissions, resolve decisions, or execute commands.")
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
        Profile::Controller => {
            let identity = client.inspect(Query::Whoami {}).await?;
            if identity["evidence"] != "managed_credential" {
                anyhow::bail!("controller MCP profile requires a managed lead credential");
            }
            let agent_id = identity["agent_id"].as_str().ok_or_else(|| {
                anyhow::anyhow!("controller MCP profile requires a managed lead credential")
            })?;
            let record = client
                .inspect(Query::AgentsInspect {
                    agent_id: AgentId(agent_id.to_string()),
                })
                .await?;
            if record["agent"]["role"] != "lead" {
                anyhow::bail!("controller MCP profile requires a managed lead credential");
            }
            let team_id = record["agent"]["team_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("managed lead has no team"))?;
            ControllerServer::new(client, agent_id.to_string(), team_id.to_string())
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
        }
        Profile::Operator => {
            let identity = client.inspect(Query::Whoami {}).await?;
            if identity["evidence"] != "managed_credential" {
                anyhow::bail!("operator MCP profile requires a managed lead credential");
            }
            let agent_id = identity["agent_id"].as_str().ok_or_else(|| {
                anyhow::anyhow!("operator MCP profile requires a managed lead credential")
            })?;
            let record = client
                .inspect(Query::AgentsInspect {
                    agent_id: AgentId(agent_id.to_string()),
                })
                .await?;
            if record["agent"]["role"] != "lead" {
                anyhow::bail!("operator MCP profile requires a managed lead credential");
            }
            OperatorServer::new(client)
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
        assert!(!worker.tool_router.has_route("assignment_create"));
        assert!(!lead.tool_router.has_route("decision_resolve"));
        assert_eq!(worker.tool_router.list_all().len(), 6);
        assert_eq!(lead.tool_router.list_all().len(), 8);
    }

    #[test]
    fn controller_catalog_is_small_and_explicit() {
        let router = ControllerServer::tool_router();
        assert_eq!(router.list_all().len(), 5);
        assert!(router.has_route("team_run_start"));
        assert!(!router.has_route("message_send"));
    }

    #[test]
    fn operator_catalog_has_only_the_four_interactive_tools() {
        let router = OperatorServer::tool_router();
        let mut names: Vec<_> = router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["team_cancel", "team_start", "team_status", "team_update"]
        );
    }
}
