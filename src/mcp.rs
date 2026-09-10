//! Stdio MCP connector. Only the separate service opens the registry database.
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
    pub run_id: String,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendArgs {
    pub run_id: String,
    pub to: String,
    pub body: String,
    pub reply_to: Option<String>,
    pub idempotency_key: String,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteArgs {
    pub run_id: String,
    pub result: String,
}

#[derive(Clone)]
pub struct InspectionServer {
    client: Client,
}

#[tool_router]
impl InspectionServer {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    async fn query(&self, query: Query) -> Result<CallToolResult, ErrorData> {
        match self.client.inspect(query).await {
            Ok(value) => Ok(CallToolResult::success(vec![ContentBlock::text(
                value.to_string(),
            )])),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    async fn action(&self, action: crate::teams::Action) -> Result<CallToolResult, ErrorData> {
        match self.client.act(action).await {
            Ok(value) => Ok(CallToolResult::success(vec![ContentBlock::text(
                value.to_string(),
            )])),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    #[tool(
        description = "Read the state, limits and completed turn outputs for an authorized run.",
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
        self.query(Query::RunsInspect {
            run_id: args.run_id,
        })
        .await
    }

    #[tool(
        description = "Inspect the ordered message history of an authorized run; does not receive or acknowledge messages.",
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
        self.query(Query::MessagesList {
            run_id: args.run_id,
        })
        .await
    }

    #[tool(
        description = "Receive and acknowledge your unread messages during your active turn. Call once at turn start; end the turn instead of polling when there is no new work.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn messages_receive(
        &self,
        Parameters(args): Parameters<RunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.action(crate::teams::Action::Receive {
            run_id: args.run_id,
        })
        .await
    }

    #[tool(
        description = "Send a bounded message to an exact teammate ID. Sender comes from your credential. For a reply, use the original message ID and original sender. Reuse an idempotency key only for an identical resend.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn messages_send(
        &self,
        Parameters(args): Parameters<SendArgs>,
    ) -> Result<CallToolResult, ErrorData> {
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
        description = "Lead only: propose the final result after teammates finish. Pending messages block completion; end your turn and let peers run. This is not independent acceptance or human approval.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn runs_complete(
        &self,
        Parameters(args): Parameters<CompleteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.action(crate::teams::Action::Complete {
            run_id: args.run_id,
            result: args.result,
        })
        .await
    }

    #[tool(
        description = "Read this connector's explicitly provisioned identity. Credential binding and native session evidence are reported separately.",
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
        description = "List groups this connector's credential may inspect. Read-only.",
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
        description = "List teams within an authorized group. Read-only; does not create or run agents.",
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
        self.query(Query::TeamsList {
            group_id: GroupId(args.group_id),
        })
        .await
    }

    #[tool(
        description = "List agent records in an authorized team. Native IDs alone do not establish liveness.",
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
        self.query(Query::AgentsList {
            team_id: TeamId(args.team_id),
        })
        .await
    }

    #[tool(
        description = "Inspect an exact authorized agent record and capability limits. Never starts or resumes execution.",
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
        self.query(Query::AgentsInspect {
            agent_id: AgentId(args.agent_id),
        })
        .await
    }
}

#[tool_handler]
impl ServerHandler for InspectionServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Agentisan registry and team messaging. Identity comes from the connector credential. Inspection is read-only. Messaging requires a managed agent's active turn and stays within its run. Receive once, perform available work, send messages, then end your turn; the scheduler resumes you on new messages. Only the lead can propose completion. These tools cannot change permissions, approve human decisions, create teams, or execute shell commands.")
    }
}

pub async fn run(client: Client) -> anyhow::Result<()> {
    InspectionServer::new(client)
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
