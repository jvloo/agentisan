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

    #[tool(
        description = "Read this connector's registered identity. Fixture bindings are simulated, not native host authentication.",
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
        description = "List simulated agent records in an authorized team. Does not establish liveness.",
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
            .with_instructions("Agentisan Milestone 1: read-only fixture registry. Identity comes from the connector credential, not your conversation ID. The tools cannot register, execute, message, approve, or resume agents. All native bindings are simulated.")
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
