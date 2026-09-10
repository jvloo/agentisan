//! Persistent team membership, authenticated mailboxes, and bounded CLI runs.
use crate::{
    fixture::private_dir,
    model::*,
    registry::{Registry, RegistryError, credential_hash},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS team_members (agent_id TEXT PRIMARY KEY REFERENCES agents(id), config TEXT NOT NULL, credential_file TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, team_id TEXT NOT NULL REFERENCES teams(id), lead_id TEXT NOT NULL, state TEXT NOT NULL, objective TEXT NOT NULL, result TEXT, error TEXT, turns INTEGER NOT NULL DEFAULT 0, max_turns INTEGER NOT NULL, max_messages INTEGER NOT NULL, deadline INTEGER NOT NULL, turn_timeout INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS one_active_run ON runs(team_id) WHERE state IN ('queued','running','completing');
CREATE TABLE IF NOT EXISTS messages (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, run_id TEXT NOT NULL REFERENCES runs(id), sender TEXT NOT NULL, recipient TEXT NOT NULL REFERENCES agents(id), body TEXT NOT NULL, reply_to TEXT, dedup_key TEXT NOT NULL, delivered_turn TEXT, created_at INTEGER NOT NULL, UNIQUE(run_id,sender,dedup_key));
CREATE TABLE IF NOT EXISTS turns (id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), agent_id TEXT NOT NULL REFERENCES agents(id), state TEXT NOT NULL, started_at INTEGER NOT NULL, ended_at INTEGER, native_id TEXT, output TEXT, usage TEXT, error TEXT, artifacts TEXT);
PRAGMA user_version=2;
";

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Codex,
}
impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberConfig {
    pub id: AgentId,
    pub name: String,
    pub role: AgentRole,
    pub provider: Provider,
    pub executable: PathBuf,
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub instructions: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamConfig {
    pub group: Group,
    pub team: Team,
    pub agents: Vec<MemberConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Send {
        run_id: String,
        to: AgentId,
        body: String,
        reply_to: Option<String>,
        idempotency_key: String,
    },
    Receive {
        run_id: String,
    },
    Complete {
        run_id: String,
        result: String,
    },
}
impl Action {
    fn run_id(&self) -> &str {
        match self {
            Self::Send { run_id, .. }
            | Self::Receive { run_id }
            | Self::Complete { run_id, .. } => run_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Work {
    pub run_id: String,
    pub turn_id: String,
    pub agent: MemberConfig,
    pub credential_file: PathBuf,
    pub native_id: Option<String>,
    pub deadline: i64,
    pub turn_timeout: u64,
    pub team_id: String,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub async fn create(registry: &Registry, data_dir: &Path, config: &TeamConfig) -> Result<Value> {
    let _admin_lock = crate::fixture::admin_lock(data_dir)?;
    if !valid_id(config.group.id.as_str())
        || !valid_id(config.team.id.as_str())
        || config.team.group_id != config.group.id
    {
        bail!("invalid group/team identity");
    }
    if config.agents.is_empty()
        || config.agents.len() > 8
        || config
            .agents
            .iter()
            .filter(|a| a.role == AgentRole::Lead)
            .count()
            != 1
    {
        bail!("a team requires exactly one lead and at most eight agents");
    }
    let lead = config
        .agents
        .iter()
        .find(|a| a.role == AgentRole::Lead)
        .expect("validated lead");
    let mut unique = std::collections::HashSet::new();
    for a in &config.agents {
        if !valid_id(a.id.as_str())
            || !unique.insert(a.id.as_str())
            || a.name.trim().is_empty()
            || a.name.len() > 256
            || a.instructions.len() > 8192
            || a.model.is_empty()
            || a.model.len() > 128
            || !["low", "medium", "high", "xhigh", "max"].contains(&a.effort.as_str())
        {
            bail!("invalid member configuration");
        }
        if !a.executable.is_absolute() || !a.executable.is_file() {
            bail!("each CLI executable must be an existing absolute path");
        }
    }
    let root = data_dir
        .join("managed")
        .join(crate::fixture::portable_component(config.team.id.as_str()));
    private_dir(&root)?;
    let mut created = Vec::new();
    let result=async {
        let mut tx=registry.pool.begin().await?;
        let exists:Option<String>=sqlx::query_scalar("SELECT payload FROM groups WHERE id=?").bind(config.group.id.as_str()).fetch_optional(&mut *tx).await?;
        let payload=serde_json::to_string(&config.group)?;
        if let Some(old)=exists {if old!=payload {bail!("group already has another definition");}}
        else {sqlx::query("INSERT INTO groups(id,payload) VALUES(?,?)").bind(config.group.id.as_str()).bind(payload).execute(&mut *tx).await?;}
        sqlx::query("INSERT INTO teams(id,group_id,payload) VALUES(?,?,?)").bind(config.team.id.as_str()).bind(config.group.id.as_str()).bind(serde_json::to_string(&config.team)?).execute(&mut *tx).await?;
        for a in &config.agents {
            let principal=new_id("principal");
            let token=format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple());
            let path=root.join(format!("{}.token",crate::fixture::portable_component(a.id.as_str())));
            let mut opts=OpenOptions::new();opts.write(true).create_new(true);
            #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;opts.mode(0o600);}
            let mut file=opts.open(&path)?;created.push(path.clone());writeln!(file,"{token}")?;file.sync_all()?;
            let agent=Agent {id:a.id.clone(),team_id:config.team.id.clone(),name:a.name.clone(),role:a.role.clone(),parent_agent_id:if a.role==AgentRole::Lead {None} else {Some(lead.id.clone())},native_binding:NativeBinding {adapter:format!("{}_cli",a.provider.as_str()),host_id:"local".into(),namespace:config.team.id.0.clone(),session_id:None,thread_id:None,subagent_id:None}};
            sqlx::query("INSERT INTO agents(id,team_id,binding_key,payload) VALUES(?,?,?,?)").bind(a.id.as_str()).bind(config.team.id.as_str()).bind(format!("pending:{}",a.id.as_str())).bind(serde_json::to_string(&agent)?).execute(&mut *tx).await?;
            let p=Principal {id:PrincipalId(principal),agent_id:Some(a.id.clone()),group_ids:vec![config.group.id.clone()]};
            sqlx::query("INSERT INTO principals(id,agent_id,token_hash,payload) VALUES(?,?,?,?)").bind(p.id.as_str()).bind(a.id.as_str()).bind(credential_hash(&token)).bind(serde_json::to_string(&p)?).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO grants(principal_id,group_id) VALUES(?,?)").bind(p.id.as_str()).bind(config.group.id.as_str()).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO team_members(agent_id,config,credential_file) VALUES(?,?,?)").bind(a.id.as_str()).bind(serde_json::to_string(a)?).bind(path.to_string_lossy().as_ref()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok::<_,anyhow::Error>(json!({"team_id":config.team.id,"agents":config.agents.iter().map(|a|&a.id).collect::<Vec<_>>(),"credential_directory":root}))
    }.await;
    if result.is_err() {
        for p in created {
            let _ = fs::remove_file(p);
        }
    }
    result
}

pub async fn start(
    registry: &Registry,
    team: &str,
    objective: &str,
    max_turns: u32,
    max_messages: u32,
    timeout: u64,
    turn_timeout: u64,
) -> Result<String> {
    if objective.trim().is_empty()
        || objective.len() > 16384
        || !(1..=100).contains(&max_turns)
        || !(1..=500).contains(&max_messages)
        || !(1..=3600).contains(&timeout)
        || !(1..=300).contains(&turn_timeout)
    {
        bail!("invalid objective or enforced limits");
    }
    let agents = sqlx::query(
        "SELECT m.config FROM team_members m JOIN agents a ON a.id=m.agent_id WHERE a.team_id=?",
    )
    .bind(team)
    .fetch_all(&registry.pool)
    .await?;
    let members: Vec<MemberConfig> = agents
        .iter()
        .map(|r| serde_json::from_str(r.get::<&str, _>("config")))
        .collect::<std::result::Result<_, _>>()?;
    let lead = members
        .iter()
        .find(|a| a.role == AgentRole::Lead)
        .ok_or_else(|| anyhow::anyhow!("managed team not found"))?;
    let id = new_id("run");
    let mut tx = registry.pool.begin().await?;
    sqlx::query("INSERT INTO runs(id,team_id,lead_id,state,objective,max_turns,max_messages,deadline,turn_timeout,created_at) VALUES(?,?,?,'queued',?,?,?,?,?,?)")
        .bind(&id).bind(team).bind(lead.id.as_str()).bind(objective).bind(max_turns).bind(max_messages).bind(now()+timeout as i64).bind(turn_timeout as i64).bind(now()).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO messages(id,run_id,sender,recipient,body,dedup_key,created_at) VALUES(?,?,'human',?,?,'initial-objective',?)")
        .bind(new_id("msg")).bind(&id).bind(lead.id.as_str()).bind(objective).bind(now()).execute(&mut *tx).await?;
    // A new objective gets fresh native sessions. Historical IDs remain in turns.
    for a in &members {
        let payload: String = sqlx::query_scalar("SELECT payload FROM agents WHERE id=?")
            .bind(a.id.as_str())
            .fetch_one(&mut *tx)
            .await?;
        let mut agent: Agent = serde_json::from_str(&payload)?;
        agent.native_binding.session_id = None;
        agent.native_binding.thread_id = None;
        sqlx::query("UPDATE agents SET payload=?,binding_key=? WHERE id=?")
            .bind(serde_json::to_string(&agent)?)
            .bind(format!("pending:{}", a.id.as_str()))
            .bind(a.id.as_str())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(id)
}

async fn caller(registry: &Registry, token: &str) -> Result<Principal, RegistryError> {
    let payload: Option<String> =
        sqlx::query_scalar("SELECT payload FROM principals WHERE token_hash=?")
            .bind(credential_hash(token))
            .fetch_optional(&registry.pool)
            .await?;
    Ok(serde_json::from_str(
        &payload.ok_or(RegistryError::Unauthorized)?,
    )?)
}

pub async fn inspect_run(
    registry: &Registry,
    token: &str,
    id: &str,
) -> Result<Value, RegistryError> {
    let p = caller(registry, token).await?;
    let row=sqlx::query("SELECT r.* FROM runs r JOIN teams t ON t.id=r.team_id JOIN grants g ON g.group_id=t.group_id WHERE r.id=? AND g.principal_id=?").bind(id).bind(p.id.as_str()).fetch_optional(&registry.pool).await?.ok_or(RegistryError::NotFound)?;
    let turns=sqlx::query("SELECT id,agent_id,state,native_id,output,usage,error,artifacts FROM turns WHERE run_id=? ORDER BY started_at,id").bind(id).fetch_all(&registry.pool).await?;
    let data:Vec<Value>=turns.iter().map(|r|json!({"id":r.get::<String,_>("id"),"agent_id":r.get::<String,_>("agent_id"),"state":r.get::<String,_>("state"),"native_id":r.get::<Option<String>,_>("native_id"),"output":r.get::<Option<String>,_>("output"),"usage":r.get::<Option<String>,_>("usage").and_then(|s|serde_json::from_str::<Value>(&s).ok()),"error":r.get::<Option<String>,_>("error")})).collect();
    Ok(
        json!({"id":id,"team_id":row.get::<String,_>("team_id"),"state":row.get::<String,_>("state"),"result":row.get::<Option<String>,_>("result"),"error":row.get::<Option<String>,_>("error"),"turn_count":row.get::<i64,_>("turns"),"max_turns":row.get::<i64,_>("max_turns"),"deadline":row.get::<i64,_>("deadline"),"turns":data,"acceptance":"not_independently_verified"}),
    )
}

pub async fn messages(registry: &Registry, token: &str, id: &str) -> Result<Value, RegistryError> {
    inspect_run(registry, token, id).await?;
    let rows = sqlx::query("SELECT * FROM messages WHERE run_id=? ORDER BY seq")
        .bind(id)
        .fetch_all(&registry.pool)
        .await?;
    Ok(json!({"messages":rows.iter().map(message_json).collect::<Vec<_>>()}))
}
fn message_json(r: &sqlx::sqlite::SqliteRow) -> Value {
    json!({"seq":r.get::<i64,_>("seq"),"id":r.get::<String,_>("id"),"run_id":r.get::<String,_>("run_id"),"from":r.get::<String,_>("sender"),"to":r.get::<String,_>("recipient"),"body":r.get::<String,_>("body"),"reply_to":r.get::<Option<String>,_>("reply_to"),"delivered_turn":r.get::<Option<String>,_>("delivered_turn")})
}

pub async fn act(registry: &Registry, token: &str, action: Action) -> Result<Value> {
    let p = caller(registry, token).await?;
    let agent = p
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("an agent binding is required"))?;
    let run = action.run_id().to_owned();
    let mut tx = registry.pool.begin().await?;
    let row=sqlx::query("SELECT r.* FROM runs r JOIN agents a ON a.team_id=r.team_id JOIN team_members m ON m.agent_id=a.id WHERE r.id=? AND a.id=?")
        .bind(&run).bind(agent.as_str()).fetch_optional(&mut *tx).await?.ok_or_else(||anyhow::anyhow!("run not found"))?;
    let state: String = row.get("state");
    if state != "running" || row.get::<i64, _>("deadline") <= now() {
        bail!("run is not accepting agent actions");
    }
    let turn: Option<String> = sqlx::query_scalar(
        "SELECT id FROM turns WHERE run_id=? AND agent_id=? AND state='running'",
    )
    .bind(&run)
    .bind(agent.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let turn = turn.ok_or_else(|| anyhow::anyhow!("no owned active turn"))?;
    let value = match action {
        Action::Send {
            to,
            body,
            reply_to,
            idempotency_key,
            ..
        } => {
            if body.trim().is_empty()
                || body.len() > 8192
                || !valid_id(&idempotency_key)
                || to == agent
            {
                bail!("invalid message body, key, or recipient");
            }
            let recipient: Option<String> =
                    sqlx::query_scalar("SELECT a.id FROM agents a JOIN team_members m ON m.agent_id=a.id WHERE a.id=? AND a.team_id=?")
                    .bind(to.as_str())
                    .bind(row.get::<String, _>("team_id"))
                    .fetch_optional(&mut *tx)
                    .await?;
            if recipient.is_none() {
                bail!("recipient not in this team");
            }
            if let Some(ref reply) = reply_to {
                let related:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE id=? AND run_id=? AND recipient=? AND sender=?").bind(reply).bind(&run).bind(agent.as_str()).bind(to.as_str()).fetch_one(&mut *tx).await?;
                if related == 0 {
                    bail!("reply must address the original sender in this run");
                }
            }
            let old =
                sqlx::query("SELECT * FROM messages WHERE run_id=? AND sender=? AND dedup_key=?")
                    .bind(&run)
                    .bind(agent.as_str())
                    .bind(&idempotency_key)
                    .fetch_optional(&mut *tx)
                    .await?;
            if let Some(old) = old {
                if old.get::<String, _>("recipient") != to.as_str()
                    || old.get::<String, _>("body") != body
                    || old.get::<Option<String>, _>("reply_to") != reply_to
                {
                    bail!("idempotency key reused for a different message");
                }
                json!({"status":"accepted","message_id":old.get::<String,_>("id"),"duplicate":true})
            } else {
                let count: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE run_id=?")
                    .bind(&run)
                    .fetch_one(&mut *tx)
                    .await?;
                if count >= row.get::<i64, _>("max_messages") {
                    bail!("message budget exhausted");
                }
                let id = new_id("msg");
                sqlx::query("INSERT INTO messages(id,run_id,sender,recipient,body,reply_to,dedup_key,created_at) VALUES(?,?,?,?,?,?,?,?)")
                    .bind(&id).bind(&run).bind(agent.as_str()).bind(to.as_str()).bind(body).bind(reply_to).bind(idempotency_key).bind(now()).execute(&mut *tx).await?;
                json!({"status":"accepted","message_id":id,"duplicate":false})
            }
        }
        Action::Receive { .. } => {
            let rows=sqlx::query("SELECT * FROM messages WHERE run_id=? AND recipient=? AND delivered_turn IS NULL ORDER BY seq").bind(&run).bind(agent.as_str()).fetch_all(&mut *tx).await?;
            sqlx::query("UPDATE messages SET delivered_turn=? WHERE run_id=? AND recipient=? AND delivered_turn IS NULL").bind(&turn).bind(&run).bind(agent.as_str()).execute(&mut *tx).await?;
            json!({"messages":rows.iter().map(message_json).collect::<Vec<_>>()})
        }
        Action::Complete { result, .. } => {
            if agent.as_str() != row.get::<String, _>("lead_id") {
                bail!("only the lead may propose completion");
            }
            if result.trim().is_empty() || result.len() > 16384 {
                bail!("invalid result");
            }
            let pending: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM messages WHERE run_id=? AND delivered_turn IS NULL",
            )
            .bind(&run)
            .fetch_one(&mut *tx)
            .await?;
            if pending > 0 {
                bail!("messages are still pending; end this turn and wait for teammates");
            }
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM turns WHERE run_id=? AND agent_id!=? AND state='running'",
            )
            .bind(&run)
            .bind(agent.as_str())
            .fetch_one(&mut *tx)
            .await?;
            let missing_reports:i64=sqlx::query_scalar("SELECT count(*) FROM team_members tm JOIN agents a ON a.id=tm.agent_id WHERE a.team_id=? AND a.id!=? AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.run_id=? AND m.sender=a.id AND m.recipient=? AND m.delivered_turn IS NOT NULL)").bind(row.get::<String,_>("team_id")).bind(agent.as_str()).bind(&run).bind(agent.as_str()).fetch_one(&mut *tx).await?;
            if active > 0 || missing_reports > 0 {
                bail!(
                    "teammates must settle and each report to the lead before completion; end this turn if work is pending"
                );
            }
            sqlx::query("UPDATE runs SET state='completing',result=? WHERE id=?")
                .bind(result)
                .bind(&run)
                .execute(&mut *tx)
                .await?;
            json!({"status":"completion_proposed","acceptance":"requires_verification"})
        }
    };
    tx.commit().await?;
    Ok(value)
}

pub async fn record_binding(
    registry: &Registry,
    run_id: &str,
    agent_id: &str,
    native_id: &str,
) -> Result<()> {
    uuid::Uuid::parse_str(native_id)?;
    let mut tx = registry.pool.begin().await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM turns WHERE run_id=? AND agent_id=? AND state='running'",
    )
    .bind(run_id)
    .bind(agent_id)
    .fetch_one(&mut *tx)
    .await?;
    if active != 1 {
        bail!("native binding requires one owned active turn");
    }
    let prior:Option<String>=sqlx::query_scalar("SELECT native_id FROM turns WHERE run_id=? AND agent_id=? AND native_id IS NOT NULL ORDER BY rowid DESC LIMIT 1").bind(run_id).bind(agent_id).fetch_optional(&mut *tx).await?;
    if prior.as_ref().is_some_and(|s| s != native_id) {
        bail!("native session changed during continuation");
    }
    let payload: String = sqlx::query_scalar("SELECT payload FROM agents WHERE id=?")
        .bind(agent_id)
        .fetch_one(&mut *tx)
        .await?;
    let mut agent: Agent = serde_json::from_str(&payload)?;
    let slot = if agent.native_binding.adapter == "claude_cli" {
        &mut agent.native_binding.session_id
    } else {
        &mut agent.native_binding.thread_id
    };
    *slot = Some(native_id.to_owned());
    sqlx::query("UPDATE turns SET native_id=? WHERE run_id=? AND agent_id=? AND state='running'")
        .bind(native_id)
        .bind(run_id)
        .bind(agent_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE agents SET payload=?,binding_key=? WHERE id=?")
        .bind(serde_json::to_string(&agent)?)
        .bind(serde_json::to_string(&agent.native_binding)?)
        .bind(agent_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn next(registry: &Registry) -> Result<Option<Work>> {
    let mut tx = registry.pool.begin().await?;
    sqlx::query("UPDATE runs SET state='exhausted',error='run deadline reached' WHERE state IN ('queued','running') AND deadline<=?").bind(now()).execute(&mut *tx).await?;
    let row=sqlx::query("SELECT r.id,r.team_id,r.deadline,r.turn_timeout,r.turns,r.max_turns,m.recipient,tm.config,tm.credential_file,a.payload FROM runs r JOIN messages m ON m.run_id=r.id JOIN team_members tm ON tm.agent_id=m.recipient JOIN agents a ON a.id=tm.agent_id WHERE r.state IN ('queued','running') AND m.delivered_turn IS NULL AND NOT EXISTS(SELECT 1 FROM turns t WHERE t.run_id=r.id AND t.state='running') ORDER BY m.seq LIMIT 1").fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        sqlx::query("UPDATE runs SET state='stalled',error='no pending messages and lead has not completed' WHERE state='running' AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.run_id=runs.id AND m.delivered_turn IS NULL) AND NOT EXISTS(SELECT 1 FROM turns t WHERE t.run_id=runs.id AND t.state='running')").execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(None);
    };
    let run_id: String = row.get("id");
    if row.get::<i64, _>("turns") >= row.get::<i64, _>("max_turns") {
        sqlx::query(
            "UPDATE runs SET state='exhausted',error='CLI invocation budget exhausted' WHERE id=?",
        )
        .bind(&run_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(None);
    }
    let member: MemberConfig = serde_json::from_str(row.get::<&str, _>("config"))?;
    let native_id:Option<String>=sqlx::query_scalar("SELECT native_id FROM turns WHERE run_id=? AND agent_id=? AND native_id IS NOT NULL ORDER BY rowid DESC LIMIT 1").bind(&run_id).bind(member.id.as_str()).fetch_optional(&mut *tx).await?;
    let turn_id = new_id("turn");
    sqlx::query("INSERT INTO turns(id,run_id,agent_id,state,started_at) VALUES(?,?,?,'running',?)")
        .bind(&turn_id)
        .bind(&run_id)
        .bind(member.id.as_str())
        .bind(now())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE runs SET state='running',turns=turns+1 WHERE id=?")
        .bind(&run_id)
        .execute(&mut *tx)
        .await?;
    let work = Work {
        run_id,
        turn_id,
        agent: member,
        credential_file: PathBuf::from(row.get::<String, _>("credential_file")),
        native_id,
        deadline: row.get("deadline"),
        turn_timeout: row.get::<i64, _>("turn_timeout") as u64,
        team_id: row.get("team_id"),
    };
    tx.commit().await?;
    Ok(Some(work))
}

pub async fn finish(
    registry: &Registry,
    work: &Work,
    result: Result<crate::worker::TurnResult>,
) -> Result<()> {
    let mut tx = registry.pool.begin().await?;
    match result {
        Ok(result) => {
            sqlx::query("UPDATE turns SET state='completed',ended_at=?,native_id=?,output=?,usage=?,artifacts=? WHERE id=?")
                .bind(now()).bind(result.native_id).bind(result.output).bind(serde_json::to_string(&result.usage)?).bind(result.artifacts.to_string_lossy().as_ref()).bind(&work.turn_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE runs SET state='completed' WHERE id=? AND state='completing'")
                .bind(&work.run_id)
                .execute(&mut *tx)
                .await?;
            let unread:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE run_id=? AND recipient=? AND delivered_turn IS NULL").bind(&work.run_id).bind(work.agent.id.as_str()).fetch_one(&mut *tx).await?;
            if unread > 0 {
                sqlx::query("UPDATE runs SET state='stalled',error='agent ended without receiving pending messages' WHERE id=? AND state='running'").bind(&work.run_id).execute(&mut *tx).await?;
            }
        }
        Err(error) => {
            let message = error.to_string();
            sqlx::query("UPDATE turns SET state='failed',ended_at=?,error=? WHERE id=?")
                .bind(now())
                .bind(&message)
                .bind(&work.turn_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE runs SET state='failed',error=? WHERE id=?")
                .bind(message)
                .bind(&work.run_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn recover_interrupted(registry: &Registry) -> Result<()> {
    let mut tx = registry.pool.begin().await?;
    sqlx::query("UPDATE runs SET state='interrupted',error='service restarted; inspect persisted messages and native sessions before retrying' WHERE state IN ('running','completing')").execute(&mut *tx).await?;
    sqlx::query("UPDATE turns SET state='unknown',error='service restarted before completion receipt' WHERE state='running'").execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Administrative continuation after inspection. Never resets budgets or replays a
/// failed/unknown native invocation; only unread messages after successful turns qualify.
pub async fn resume(registry: &Registry, id: &str) -> Result<()> {
    let mut tx = registry.pool.begin().await?;
    let row = sqlx::query("SELECT state,deadline,turns,max_turns FROM runs WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run not found"))?;
    if row.get::<String, _>("state") != "stalled"
        || row.get::<i64, _>("deadline") <= now()
        || row.get::<i64, _>("turns") >= row.get::<i64, _>("max_turns")
    {
        bail!("only stalled runs with remaining original limits can resume");
    }
    let uncertain: i64 =
        sqlx::query_scalar("SELECT count(*) FROM turns WHERE run_id=? AND state!='completed'")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM messages WHERE run_id=? AND delivered_turn IS NULL",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if uncertain != 0 || pending == 0 {
        bail!("run requires reconciliation; there must be unread messages and no uncertain turns");
    }
    sqlx::query("UPDATE runs SET state='running',error=NULL WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
