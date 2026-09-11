use crate::model::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("registration conflict: {0}")]
    Conflict(String),
    #[error("invalid fixture: {0}")]
    Invalid(String),
    #[error("registry storage failure")]
    Storage(#[from] sqlx::Error),
    #[error("invalid stored record")]
    Serialization(#[from] serde_json::Error),
}
type Result<T> = std::result::Result<T, RegistryError>;

#[derive(Clone)]
pub struct Registry {
    pub(crate) pool: SqlitePool,
}

pub fn credential_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS groups (id TEXT PRIMARY KEY, payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS teams (id TEXT PRIMARY KEY, group_id TEXT NOT NULL REFERENCES groups(id), payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS agents (id TEXT PRIMARY KEY, team_id TEXT NOT NULL REFERENCES teams(id), binding_key TEXT NOT NULL UNIQUE, payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS principals (id TEXT PRIMARY KEY, agent_id TEXT REFERENCES agents(id), token_hash TEXT NOT NULL UNIQUE, payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS grants (principal_id TEXT NOT NULL REFERENCES principals(id), group_id TEXT NOT NULL REFERENCES groups(id), PRIMARY KEY(principal_id,group_id));
PRAGMA user_version=1;
";

impl Registry {
    pub async fn open(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *tx)
            .await?;
        match version {
            0 => {
                sqlx::raw_sql(SCHEMA).execute(&mut *tx).await?;
            }
            1..=5 => {}
            _ => {
                return Err(RegistryError::Invalid(
                    "unsupported database schema version".into(),
                ));
            }
        }
        if version < 2 {
            sqlx::raw_sql(crate::teams::SCHEMA)
                .execute(&mut *tx)
                .await?;
        }
        if version < 3 {
            sqlx::raw_sql("CREATE TABLE IF NOT EXISTS registry_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT OR IGNORE INTO registry_meta(key,value) SELECT 'initialized','1' WHERE EXISTS(SELECT 1 FROM groups); PRAGMA user_version=3;").execute(&mut *tx).await?;
        }
        if version < 4 {
            sqlx::raw_sql(crate::verification::SCHEMA)
                .execute(&mut *tx)
                .await?;
        }
        if version < 5 {
            sqlx::raw_sql(crate::teams::LEASE_SCHEMA)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Self { pool })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub async fn is_initialized(&self) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM registry_meta WHERE key='initialized' AND value='1'",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(count == 1)
    }

    pub async fn register_fixture(
        &self,
        fixture: &Fixture,
        credentials: &[CredentialHash],
    ) -> Result<()> {
        validate(fixture, credentials)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for group in &fixture.groups {
            let payload = serde_json::to_string(group)?;
            if absent_or_equal(&mut tx, "groups", group.id.as_str(), &payload).await? {
                sqlx::query("INSERT INTO groups(id,payload) VALUES(?,?)")
                    .bind(group.id.as_str())
                    .bind(payload)
                    .execute(&mut *tx)
                    .await
                    .map_err(constraint)?;
            }
        }
        for team in &fixture.teams {
            let payload = serde_json::to_string(team)?;
            if absent_or_equal(&mut tx, "teams", team.id.as_str(), &payload).await? {
                sqlx::query("INSERT INTO teams(id,group_id,payload) VALUES(?,?,?)")
                    .bind(team.id.as_str())
                    .bind(team.group_id.as_str())
                    .bind(payload)
                    .execute(&mut *tx)
                    .await
                    .map_err(constraint)?;
            }
        }
        for agent in &fixture.agents {
            let payload = serde_json::to_string(agent)?;
            if absent_or_equal(&mut tx, "agents", agent.id.as_str(), &payload).await? {
                sqlx::query("INSERT INTO agents(id,team_id,binding_key,payload) VALUES(?,?,?,?)")
                    .bind(agent.id.as_str())
                    .bind(agent.team_id.as_str())
                    .bind(serde_json::to_string(&agent.native_binding)?)
                    .bind(payload)
                    .execute(&mut *tx)
                    .await
                    .map_err(constraint)?;
            }
        }
        let hashes: HashMap<_, _> = credentials
            .iter()
            .map(|c| (c.principal_id.as_str(), c.sha256.as_str()))
            .collect();
        for original in &fixture.principals {
            let mut principal = original.clone();
            principal
                .group_ids
                .sort_by(|a, b| a.as_str().cmp(b.as_str()));
            let payload = serde_json::to_string(&principal)?;
            let hash = hashes
                .get(principal.id.as_str())
                .ok_or_else(|| RegistryError::Invalid("missing credential".into()))?;
            if absent_or_equal(&mut tx, "principals", principal.id.as_str(), &payload).await? {
                sqlx::query(
                    "INSERT INTO principals(id,agent_id,token_hash,payload) VALUES(?,?,?,?)",
                )
                .bind(principal.id.as_str())
                .bind(principal.agent_id.as_ref().map(AgentId::as_str))
                .bind(*hash)
                .bind(payload)
                .execute(&mut *tx)
                .await
                .map_err(constraint)?;
                for group in &principal.group_ids {
                    sqlx::query("INSERT INTO grants(principal_id,group_id) VALUES(?,?)")
                        .bind(principal.id.as_str())
                        .bind(group.as_str())
                        .execute(&mut *tx)
                        .await?;
                }
            } else {
                let old: String =
                    sqlx::query_scalar("SELECT token_hash FROM principals WHERE id=?")
                        .bind(principal.id.as_str())
                        .fetch_one(&mut *tx)
                        .await?;
                if old != *hash {
                    return Err(RegistryError::Conflict(
                        "credential cannot be replaced by fixture import".into(),
                    ));
                }
            }
        }
        sqlx::query("INSERT OR IGNORE INTO registry_meta(key,value) VALUES('initialized','1')")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn inspect(&self, token: Option<&str>, query: Query) -> Result<Value> {
        let Some(token) = token else {
            return if matches!(query, Query::Whoami {}) {
                Ok(json!({"status":"unbound","reason":"No credential supplied"}))
            } else {
                Err(RegistryError::Unauthorized)
            };
        };
        if token.is_empty() {
            return Err(RegistryError::Unauthorized);
        }
        let payload: Option<String> =
            sqlx::query_scalar("SELECT payload FROM principals WHERE token_hash=?")
                .bind(credential_hash(token))
                .fetch_optional(&self.pool)
                .await?;
        let principal: Principal =
            serde_json::from_str(&payload.ok_or(RegistryError::Unauthorized)?)?;
        let lease = sqlx::query("SELECT l.turn_id,l.agent_id,l.epoch,l.state,l.expires_at,t.run_id FROM turn_leases l JOIN turns t ON t.id=l.turn_id WHERE l.principal_id=?")
            .bind(principal.id.as_str()).fetch_optional(&self.pool).await?;
        match query {
            Query::Whoami {} => {
                let managed: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM team_members WHERE agent_id=?")
                        .bind(principal.agent_id.as_ref().map(AgentId::as_str))
                        .fetch_one(&self.pool)
                        .await?;
                let lease_context = lease.as_ref().map(|row| {
                    json!({
                        "turn_id":row.get::<String,_>("turn_id"),
                        "run_id":row.get::<String,_>("run_id"),
                        "ownership_epoch":row.get::<i64,_>("epoch"),
                        "state":row.get::<String,_>("state"),
                        "expires_at":row.get::<i64,_>("expires_at")
                    })
                });
                Ok(
                    json!({"status":if principal.agent_id.is_some() {"bound"} else {"unbound"},"principal_id":principal.id,"agent_id":principal.agent_id,"evidence":if lease.is_some() {"turn_lease"} else if managed>0 {"managed_credential"} else {"fixture_credential"},"source":if lease.is_some() {"managed_turn"} else if managed>0 {"managed_cli"} else {"fixture"},"lease":lease_context}),
                )
            }
            Query::GroupsList {} => {
                if lease.is_some() {
                    return Err(RegistryError::NotFound);
                }
                let rows = sqlx::query("SELECT g.payload FROM groups g JOIN grants p ON p.group_id=g.id WHERE p.principal_id=? ORDER BY g.id")
                    .bind(principal.id.as_str()).fetch_all(&self.pool).await?;
                Ok(json!({"groups":decode_rows::<Group>(rows)?}))
            }
            Query::TeamsList { group_id } => {
                if lease.is_some() {
                    return Err(RegistryError::NotFound);
                }
                self.require_group(principal.id.as_str(), group_id.as_str())
                    .await?;
                let rows = sqlx::query("SELECT payload FROM teams WHERE group_id=? ORDER BY id")
                    .bind(group_id.as_str())
                    .fetch_all(&self.pool)
                    .await?;
                Ok(json!({"teams":decode_rows::<Team>(rows)?}))
            }
            Query::AgentsList { team_id } => {
                if lease.is_some() {
                    return Err(RegistryError::NotFound);
                }
                let group: Option<String> =
                    sqlx::query_scalar("SELECT group_id FROM teams WHERE id=?")
                        .bind(team_id.as_str())
                        .fetch_optional(&self.pool)
                        .await?;
                self.require_group(
                    principal.id.as_str(),
                    &group.ok_or(RegistryError::NotFound)?,
                )
                .await?;
                let rows = sqlx::query("SELECT payload FROM agents WHERE team_id=? ORDER BY id")
                    .bind(team_id.as_str())
                    .fetch_all(&self.pool)
                    .await?;
                Ok(json!({"agents":decode_rows::<Agent>(rows)?}))
            }
            Query::AgentsInspect { agent_id } => {
                if lease
                    .as_ref()
                    .is_some_and(|row| row.get::<String, _>("agent_id") != agent_id.as_str())
                {
                    return Err(RegistryError::NotFound);
                }
                let payload: Option<String> = sqlx::query_scalar("SELECT a.payload FROM agents a JOIN teams t ON t.id=a.team_id JOIN grants p ON p.group_id=t.group_id WHERE a.id=? AND p.principal_id=?")
                    .bind(agent_id.as_str()).bind(principal.id.as_str()).fetch_optional(&self.pool).await?;
                let agent: Agent = serde_json::from_str(&payload.ok_or(RegistryError::NotFound)?)?;
                let managed: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM team_members WHERE agent_id=?")
                        .bind(agent.id.as_str())
                        .fetch_one(&self.pool)
                        .await?;
                if managed > 0 {
                    let latest_run=sqlx::query("SELECT id,state,created_at FROM runs WHERE team_id=? ORDER BY created_at DESC,rowid DESC LIMIT 1")
                        .bind(agent.team_id.as_str()).fetch_optional(&self.pool).await?;
                    let activity = if let Some(run) = latest_run {
                        let run_id: String = run.get("id");
                        let run_state: String = run.get("state");
                        let turn=sqlx::query("SELECT id,state,started_at FROM turns WHERE run_id=? AND agent_id=? ORDER BY rowid DESC LIMIT 1")
                            .bind(&run_id).bind(agent.id.as_str()).fetch_optional(&self.pool).await?;
                        let running = turn
                            .as_ref()
                            .is_some_and(|row| row.get::<String, _>("state") == "running");
                        json!({
                            "source":"agentisan_runtime",
                            "authoritative":true,
                            "state":if running {"running"} else if matches!(run_state.as_str(),"queued"|"running"|"completing") {"waiting"} else {"idle"},
                            "run_id":run_id,
                            "run_state":run_state,
                            "turn_id":turn.as_ref().map(|row|row.get::<String,_>("id")),
                            "turn_state":turn.as_ref().map(|row|row.get::<String,_>("state")),
                            "since":turn.as_ref().map(|row|row.get::<i64,_>("started_at")).unwrap_or_else(||run.get::<i64,_>("created_at")),
                            "native_client_status":"advisory_while_agentisan_owns_the_run"
                        })
                    } else {
                        json!({"source":"agentisan_runtime","authoritative":true,"state":"not_started","native_client_status":"advisory"})
                    };
                    return Ok(
                        json!({"agent":agent,"source":"managed_cli","activity":activity,"capabilities":{"inspect":true,"messages":true,"native_open":false,"native_resume":false,"shell_execution":false}}),
                    );
                }
                Ok(
                    json!({"agent":agent,"source":"fixture","connection":"unverified","activity":"unobserved","capabilities":{"inspect":true,"native_open":false,"native_resume":false,"execute":false}}),
                )
            }
            Query::RunsInspect { run_id } => {
                if lease.is_some() {
                    return Err(RegistryError::NotFound);
                }
                crate::teams::inspect_run(self, token, &run_id).await
            }
            Query::MessagesList { run_id } => {
                if lease.is_some() {
                    return Err(RegistryError::NotFound);
                }
                crate::teams::messages(self, token, &run_id).await
            }
        }
    }

    async fn require_group(&self, principal: &str, group: &str) -> Result<()> {
        let permitted: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM grants WHERE principal_id=? AND group_id=?")
                .bind(principal)
                .bind(group)
                .fetch_one(&self.pool)
                .await?;
        if permitted == 0 {
            return Err(RegistryError::NotFound);
        }
        Ok(())
    }
}

fn decode_rows<T: DeserializeOwned + Serialize>(
    rows: Vec<sqlx::sqlite::SqliteRow>,
) -> Result<Vec<T>> {
    rows.into_iter()
        .map(|row| Ok(serde_json::from_str(row.try_get::<&str, _>("payload")?)?))
        .collect()
}

fn constraint(error: sqlx::Error) -> RegistryError {
    if error
        .as_database_error()
        .is_some_and(|e| e.is_unique_violation())
    {
        RegistryError::Conflict("duplicate identity or credential".into())
    } else {
        RegistryError::Storage(error)
    }
}

async fn absent_or_equal(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    id: &str,
    payload: &str,
) -> Result<bool> {
    let sql = match table {
        "groups" => "SELECT payload FROM groups WHERE id=?",
        "teams" => "SELECT payload FROM teams WHERE id=?",
        "agents" => "SELECT payload FROM agents WHERE id=?",
        "principals" => "SELECT payload FROM principals WHERE id=?",
        _ => return Err(invalid("unknown registry table")),
    };
    let old: Option<String> = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    match old {
        None => Ok(true),
        Some(old) if old == payload => Ok(false),
        Some(_) => Err(RegistryError::Conflict(format!(
            "{table} record {id} already exists with a different definition"
        ))),
    }
}

fn invalid(message: &str) -> RegistryError {
    RegistryError::Invalid(message.to_owned())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

fn validate(f: &Fixture, credentials: &[CredentialHash]) -> Result<()> {
    if f.schema_version != 1 {
        return Err(invalid("unsupported fixture schema version"));
    }
    if f.groups.len() + f.teams.len() + f.agents.len() + f.principals.len() > 10_000 {
        return Err(invalid("too many records"));
    }
    let groups: HashSet<_> = f.groups.iter().map(|g| g.id.as_str()).collect();
    let teams: HashMap<_, _> = f.teams.iter().map(|t| (t.id.as_str(), t)).collect();
    let agents: HashMap<_, _> = f.agents.iter().map(|a| (a.id.as_str(), a)).collect();
    let principals: HashSet<_> = f.principals.iter().map(|p| p.id.as_str()).collect();
    if groups.len() != f.groups.len()
        || teams.len() != f.teams.len()
        || agents.len() != f.agents.len()
        || principals.len() != f.principals.len()
    {
        return Err(invalid("duplicate canonical ID"));
    }
    for g in &f.groups {
        if !valid_id(g.id.as_str()) || !valid_text(&g.name, 256) {
            return Err(invalid("invalid group"));
        }
    }
    for t in &f.teams {
        if !valid_id(t.id.as_str())
            || !valid_text(&t.name, 256)
            || !groups.contains(t.group_id.as_str())
        {
            return Err(invalid("invalid team or group reference"));
        }
    }
    let mut bindings = HashSet::new();
    for a in &f.agents {
        if !valid_id(a.id.as_str())
            || !valid_text(&a.name, 256)
            || !teams.contains_key(a.team_id.as_str())
        {
            return Err(invalid("invalid agent or team reference"));
        }
        let b = &a.native_binding;
        if b.adapter != "fake" || !valid_text(&b.host_id, 512) || !valid_text(&b.namespace, 512) {
            return Err(invalid("only explicit fake-adapter bindings are supported"));
        }
        let ids = [&b.session_id, &b.thread_id, &b.subagent_id];
        if ids.iter().all(|id| id.is_none())
            || ids
                .iter()
                .any(|id| id.as_ref().is_some_and(|v| !valid_text(v, 512)))
        {
            return Err(invalid("invalid native identifier"));
        }
        if !bindings.insert(serde_json::to_string(b)?) {
            return Err(RegistryError::Conflict("duplicate native binding".into()));
        }
        let mut seen = HashSet::from([a.id.as_str()]);
        let mut next = a.parent_agent_id.as_ref();
        while let Some(id) = next {
            if !seen.insert(id.as_str()) {
                return Err(invalid("cyclic parentage"));
            }
            let parent = agents
                .get(id.as_str())
                .ok_or_else(|| invalid("unknown parent"))?;
            if parent.team_id != a.team_id {
                return Err(invalid("parent belongs to another team"));
            }
            next = parent.parent_agent_id.as_ref();
        }
    }
    for p in &f.principals {
        if !valid_id(p.id.as_str()) {
            return Err(invalid("invalid principal ID"));
        }
        let grants: HashSet<_> = p.group_ids.iter().map(GroupId::as_str).collect();
        if grants.len() != p.group_ids.len() || grants.iter().any(|g| !groups.contains(g)) {
            return Err(invalid("invalid group grants"));
        }
        if let Some(id) = &p.agent_id {
            let agent = agents
                .get(id.as_str())
                .ok_or_else(|| invalid("unknown bound agent"))?;
            let team = teams
                .get(agent.team_id.as_str())
                .ok_or_else(|| invalid("unknown bound team"))?;
            if !grants.contains(team.group_id.as_str()) {
                return Err(invalid("bound agent group is not granted"));
            }
        }
    }
    let mut hashes = HashSet::new();
    let mut credential_ids = HashSet::new();
    for c in credentials {
        if !principals.contains(c.principal_id.as_str())
            || !credential_ids.insert(c.principal_id.as_str())
            || c.sha256.len() != 64
            || !c
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !hashes.insert(c.sha256.as_str())
        {
            return Err(invalid("invalid or duplicate credential mapping"));
        }
    }
    if credential_ids.len() != principals.len() {
        return Err(invalid("missing credential"));
    }
    Ok(())
}
