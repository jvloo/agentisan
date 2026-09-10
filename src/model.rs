//! Wire types for the simulated-agent registry. Native IDs are never canonical IDs.
use serde::{Deserialize, Serialize};

macro_rules! id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}
id!(GroupId);
id!(TeamId);
id!(AgentId);
id!(PrincipalId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Team {
    pub id: TeamId,
    pub group_id: GroupId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Lead,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeBinding {
    pub adapter: String,
    pub host_id: String,
    pub namespace: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub subagent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub id: AgentId,
    pub team_id: TeamId,
    pub name: String,
    pub role: AgentRole,
    pub parent_agent_id: Option<AgentId>,
    pub native_binding: NativeBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub id: PrincipalId,
    pub agent_id: Option<AgentId>,
    pub group_ids: Vec<GroupId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub schema_version: u32,
    pub groups: Vec<Group>,
    pub teams: Vec<Team>,
    pub agents: Vec<Agent>,
    pub principals: Vec<Principal>,
}

/// Created by the trusted local initializer, never accepted as an inspection argument.
#[derive(Debug, Clone)]
pub struct CredentialHash {
    pub principal_id: PrincipalId,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Whoami {},
    GroupsList {},
    TeamsList { group_id: GroupId },
    AgentsList { team_id: TeamId },
    AgentsInspect { agent_id: AgentId },
}
