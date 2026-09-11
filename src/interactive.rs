//! Interactive coordination (v3 phase 1): an authenticated managed-lead credential
//! starts and controls a run directly, without launching the configured native lead.
//! Workers still use the existing turn-lease, message, and commit machinery in
//! `teams`/`assignments` unchanged; this module only adds the controller-side
//! surface (`team_start`, `team_status`, `team_update`, `team_cancel`) and the
//! control-handle/lease/version fencing that guards its mutations.
use crate::{
    model::AgentRole,
    registry::Registry,
    teams::{MemberConfig, caller, new_id, now, valid_id, validate_start},
};
use anyhow::{Result, bail};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};
use std::time::{Duration, Instant};

/// Additive, forward-only migration after v8. Existing v8 records remain readable;
/// pre-migration runs are labeled `autonomous_legacy` by the column default so new
/// interactive/autonomous inserts (which always specify `mode` explicitly) are not
/// confused with runs that predate coordination-mode tracking.
pub const SCHEMA: &str = "
ALTER TABLE runs ADD COLUMN mode TEXT NOT NULL DEFAULT 'autonomous_legacy';
ALTER TABLE runs ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE messages ADD COLUMN controller_ack_at INTEGER;
DROP INDEX one_active_run;
CREATE UNIQUE INDEX one_active_run ON runs(team_id) WHERE state IN ('queued','running','completing','waiting_for_controller','waiting_for_human','cancel_requested');
CREATE TABLE IF NOT EXISTS run_workers (
    run_id TEXT NOT NULL REFERENCES runs(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    PRIMARY KEY(run_id,agent_id)
);
CREATE TABLE IF NOT EXISTS control_capabilities (
    run_id TEXT PRIMARY KEY REFERENCES runs(id),
    handle_hash TEXT NOT NULL,
    epoch INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'active',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS controller_leases (
    run_id TEXT PRIMARY KEY REFERENCES runs(id),
    connector_instance TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS controller_start_requests (
    principal_id TEXT NOT NULL REFERENCES principals(id),
    idempotency_key TEXT NOT NULL,
    request TEXT NOT NULL,
    run_id TEXT NOT NULL UNIQUE REFERENCES runs(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(principal_id,idempotency_key)
);
CREATE TABLE IF NOT EXISTS controller_mutations (
    run_id TEXT NOT NULL REFERENCES runs(id),
    idempotency_key TEXT NOT NULL,
    request TEXT NOT NULL,
    receipt TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(run_id,idempotency_key)
);
PRAGMA user_version=9;
";

/// Controller leases are short-lived; an expired lease can be reacquired by any
/// holder of the run's control handle, incrementing the coordination epoch and
/// fencing stale writers bound to the previous holder.
const LEASE_TTL_SECONDS: i64 = 300;
const MAX_WORKERS: usize = 7;
const MAX_INITIAL_WORK: usize = 16;

fn valid_connector(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_handle(handle: &str) -> bool {
    !handle.is_empty() && handle.len() <= 256
}

fn handle_hash(handle: &str) -> String {
    format!("{:x}", Sha256::digest(handle.as_bytes()))
}

/// Deterministic per-run capability derived from the controller credential so an
/// exact authorized `team_start` retry can reproduce the same handle without the
/// database ever storing the plaintext value.
fn derive_handle(controller_secret: &str, run_id: &str, epoch: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"agentisan-interactive-control-v1:");
    hasher.update(controller_secret.as_bytes());
    hasher.update(b":");
    hasher.update(run_id.as_bytes());
    hasher.update(b":");
    hasher.update(epoch.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitialWorkItem {
    /// Exact configured worker agent ID; must be one of `workers`.
    pub assignee: String,
    pub objective: String,
    pub done_criteria: Vec<String>,
}

/// Starts an interactive run directly from the authenticated managed-lead
/// credential. The configured lead is never queued for native execution.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartArgs {
    pub objective: String,
    /// Must be true to authorize actual provider calls for the selected workers.
    pub live: bool,
    /// Stable identity for this MCP connector process, generated once at connection.
    pub connector_instance: String,
    /// Exact configured worker agent IDs to engage. Never includes the lead.
    pub workers: Vec<String>,
    /// Bounded initial work; every entry's assignee must be in `workers`.
    pub initial_work: Vec<InitialWorkItem>,
    pub idempotency_key: String,
    pub max_turns: Option<u32>,
    pub max_messages: Option<u32>,
    pub timeout_seconds: Option<u64>,
    pub turn_timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct NormalizedStart<'a> {
    objective: &'a str,
    workers: &'a [String],
    initial_work: &'a [InitialWorkItem],
    max_turns: u32,
    max_messages: u32,
    timeout_seconds: u64,
    turn_timeout_seconds: u64,
}

fn valid_work_item(item: &InitialWorkItem, workers: &[String]) -> bool {
    workers.contains(&item.assignee)
        && !item.objective.trim().is_empty()
        && item.objective.len() <= 4096
        && !item.done_criteria.is_empty()
        && item.done_criteria.len() <= 16
        && item
            .done_criteria
            .iter()
            .all(|c| !c.trim().is_empty() && c.len() <= 512)
}

pub async fn start(registry: &Registry, token: &str, args: StartArgs) -> Result<Value> {
    let principal = caller(registry, token).await?;
    let agent_id = principal
        .agent_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("controller credential must belong to a managed lead"))?;
    let row = sqlx::query(
        "SELECT a.team_id,m.config FROM agents a JOIN team_members m ON m.agent_id=a.id WHERE a.id=?",
    )
    .bind(agent_id.as_str())
    .fetch_optional(&registry.pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("controller credential must belong to a managed lead"))?;
    let lead: MemberConfig = serde_json::from_str(row.get("config"))?;
    if lead.role != AgentRole::Lead {
        bail!("controller credential must belong to a managed lead");
    }
    if !args.live {
        bail!("live authorization is required");
    }
    if !valid_id(&args.idempotency_key) || !valid_connector(&args.connector_instance) {
        bail!("invalid request identifiers");
    }
    let max_turns = args.max_turns.unwrap_or(crate::teams::DEFAULT_MAX_TURNS);
    let max_messages = args
        .max_messages
        .unwrap_or(crate::teams::DEFAULT_MAX_MESSAGES);
    let timeout_seconds = args
        .timeout_seconds
        .unwrap_or(crate::teams::DEFAULT_TIMEOUT_SECONDS);
    let turn_timeout_seconds = args
        .turn_timeout_seconds
        .unwrap_or(crate::teams::DEFAULT_TURN_TIMEOUT_SECONDS);
    validate_start(
        &args.objective,
        max_turns,
        max_messages,
        timeout_seconds,
        turn_timeout_seconds,
    )?;
    let mut workers = args.workers.clone();
    workers.sort();
    workers.dedup();
    if workers.is_empty()
        || workers.len() > MAX_WORKERS
        || workers.len() != args.workers.len()
        || workers.iter().any(|w| !valid_id(w) || *w == agent_id.0)
    {
        bail!("invalid worker selection");
    }
    if args.initial_work.is_empty()
        || args.initial_work.len() > MAX_INITIAL_WORK
        || args
            .initial_work
            .iter()
            .any(|item| !valid_work_item(item, &args.workers))
    {
        bail!("invalid or empty bounded initial work");
    }
    if workers.iter().any(|worker| {
        !args
            .initial_work
            .iter()
            .any(|item| &item.assignee == worker)
    }) {
        bail!("each engaged worker requires initial work");
    }
    let team_id: String = row.get("team_id");
    let members = sqlx::query(
        "SELECT a.id,m.config FROM agents a JOIN team_members m ON m.agent_id=a.id WHERE a.team_id=?",
    )
    .bind(&team_id)
    .fetch_all(&registry.pool)
    .await?;
    for worker in &workers {
        let found = members.iter().any(|r| {
            r.get::<String, _>("id") == *worker
                && serde_json::from_str::<MemberConfig>(r.get("config"))
                    .is_ok_and(|m| m.role == AgentRole::Worker)
        });
        if !found {
            bail!("worker is not a configured member of this team");
        }
    }

    let normalized = NormalizedStart {
        objective: &args.objective,
        workers: &args.workers,
        initial_work: &args.initial_work,
        max_turns,
        max_messages,
        timeout_seconds,
        turn_timeout_seconds,
    };
    let request = serde_json::to_string(&normalized)?;
    let mut tx = registry.pool.begin_with("BEGIN IMMEDIATE").await?;
    let prior = sqlx::query(
        "SELECT q.request,q.run_id,r.state,r.version FROM controller_start_requests q JOIN runs r ON r.id=q.run_id WHERE q.principal_id=? AND q.idempotency_key=?",
    )
    .bind(principal.id.as_str())
    .bind(&args.idempotency_key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(prior) = prior {
        if prior.get::<String, _>("request") != request {
            bail!("idempotency key reused for another run request");
        }
        let run_id: String = prior.get("run_id");
        let handle = derive_handle(token, &run_id, 1);
        let epoch: i64 = sqlx::query_scalar("SELECT epoch FROM controller_leases WHERE run_id=?")
            .bind(&run_id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(json!({
            "run_id": run_id,
            "team_id": team_id,
            "mode": "interactive",
            "state": prior.get::<String, _>("state"),
            "version": prior.get::<i64, _>("version"),
            "coordination_epoch": epoch,
            "control_handle": handle,
            "workers": workers,
            "duplicate": true,
            "controller_identity": "managed_team_lead_credential",
            "chat_identity": "not_asserted"
        }));
    }

    let run_id = new_id("run");
    let inserted = sqlx::query(
        "INSERT INTO runs(id,team_id,lead_id,state,objective,max_turns,max_messages,deadline,turn_timeout,created_at,mode,version) VALUES(?,?,?,'queued',?,?,?,?,?,?,'interactive',1)",
    )
    .bind(&run_id)
    .bind(&team_id)
    .bind(agent_id.as_str())
    .bind(&args.objective)
    .bind(max_turns)
    .bind(max_messages)
    .bind(now() + timeout_seconds as i64)
    .bind(turn_timeout_seconds as i64)
    .bind(now())
    .execute(&mut *tx)
    .await;
    if let Err(error) = inserted {
        if error
            .as_database_error()
            .is_some_and(|value| value.is_unique_violation())
        {
            bail!("team already has an active run");
        }
        return Err(error.into());
    }
    for worker in &workers {
        sqlx::query("INSERT INTO run_workers(run_id,agent_id) VALUES(?,?)")
            .bind(&run_id)
            .bind(worker)
            .execute(&mut *tx)
            .await?;
    }
    for (index, item) in args.initial_work.iter().enumerate() {
        sqlx::query(
            "INSERT INTO messages(id,run_id,sender,recipient,body,dedup_key,created_at) VALUES(?,?,'controller',?,?,?,?)",
        )
        .bind(new_id("msg"))
        .bind(&run_id)
        .bind(&item.assignee)
        .bind(json!({"kind":"initial_work","objective":item.objective,"done_criteria":item.done_criteria}).to_string())
        .bind(format!("initial-work-{index}"))
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    let handle = derive_handle(token, &run_id, 1);
    sqlx::query(
        "INSERT INTO control_capabilities(run_id,handle_hash,epoch,status,created_at) VALUES(?,?,1,'active',?)",
    )
    .bind(&run_id)
    .bind(handle_hash(&handle))
    .bind(now())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO controller_leases(run_id,connector_instance,epoch,expires_at,created_at) VALUES(?,?,1,?,?)",
    )
    .bind(&run_id)
    .bind(&args.connector_instance)
    .bind(now() + LEASE_TTL_SECONDS)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO controller_start_requests(principal_id,idempotency_key,request,run_id,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(principal.id.as_str())
    .bind(&args.idempotency_key)
    .bind(&request)
    .bind(&run_id)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({
        "run_id": run_id,
        "team_id": team_id,
        "mode": "interactive",
        "state": "queued",
        "version": 1,
        "coordination_epoch": 1,
        "control_handle": handle,
        "workers": workers,
        "duplicate": false,
        "controller_identity": "managed_team_lead_credential",
        "chat_identity": "not_asserted"
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    pub run_id: String,
    pub after_cursor: Option<String>,
    /// Bounded 0..=60. Zero returns an immediate snapshot; never triggers model work.
    pub timeout_seconds: Option<u64>,
    /// Fetch one exact committed message body after discovering its ID from a preview.
    pub message_id: Option<String>,
}

async fn snapshot(
    registry: &Registry,
    agent_id: &str,
    run_id: &str,
    message_id: Option<&str>,
) -> Result<Value> {
    let run = sqlx::query("SELECT * FROM runs WHERE id=?")
        .bind(run_id)
        .fetch_optional(&registry.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run not found"))?;
    if run.get::<String, _>("mode") != "interactive" || run.get::<String, _>("lead_id") != agent_id
    {
        bail!("run not found");
    }
    let version: i64 = run.get("version");
    let state: String = run.get("state");
    let max_seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(seq),0) FROM messages WHERE run_id=? AND staged_turn IS NULL",
    )
    .bind(run_id)
    .fetch_one(&registry.pool)
    .await?;
    let max_turn_rowid: i64 =
        sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM turns WHERE run_id=?")
            .bind(run_id)
            .fetch_one(&registry.pool)
            .await?;
    let max_turn_finished: i64 =
        sqlx::query_scalar("SELECT COALESCE(max(ended_at),0) FROM turns WHERE run_id=?")
            .bind(run_id)
            .fetch_one(&registry.pool)
            .await?;
    let coordination_epoch: i64 =
        sqlx::query_scalar("SELECT epoch FROM controller_leases WHERE run_id=?")
            .bind(run_id)
            .fetch_one(&registry.pool)
            .await?;
    let cursor = format!(
        "{:x}",
        Sha256::digest(
            format!("{version}:{max_seq}:{max_turn_rowid}:{max_turn_finished}:{state}").as_bytes()
        )
    );
    let turns = sqlx::query(
        "SELECT id,agent_id,state,started_at,ended_at FROM turns WHERE run_id=? ORDER BY started_at,id",
    )
    .bind(run_id)
    .fetch_all(&registry.pool)
    .await?;
    let inbox = sqlx::query(
        "SELECT m.id,m.sender,substr(m.body,1,1024) AS body_preview,length(m.body)>1024 AS body_truncated,m.created_at,m.controller_ack_at FROM messages m WHERE m.run_id=? AND m.recipient=? AND m.staged_turn IS NULL AND m.sender IN (SELECT agent_id FROM run_workers WHERE run_id=m.run_id) AND m.seq=(SELECT max(latest.seq) FROM messages latest WHERE latest.run_id=m.run_id AND latest.sender=m.sender AND latest.recipient=m.recipient AND latest.staged_turn IS NULL) ORDER BY m.seq",
    )
    .bind(run_id)
    .bind(agent_id)
    .fetch_all(&registry.pool)
    .await?;
    let workers: Vec<String> =
        sqlx::query_scalar("SELECT agent_id FROM run_workers WHERE run_id=?")
            .bind(run_id)
            .fetch_all(&registry.pool)
            .await?;
    let messages = sqlx::query(
        "SELECT id,sender,recipient,substr(body,1,256) AS body_preview,length(body)>256 AS body_truncated,reply_to,created_at FROM messages WHERE run_id=? AND staged_turn IS NULL ORDER BY seq DESC LIMIT 64",
    )
    .bind(run_id)
    .fetch_all(&registry.pool)
    .await?;
    let message_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM messages WHERE run_id=? AND staged_turn IS NULL")
            .bind(run_id)
            .fetch_one(&registry.pool)
            .await?;
    let message_detail = if let Some(message_id) = message_id {
        sqlx::query(
            "SELECT id,sender,recipient,body,reply_to,created_at FROM messages WHERE id=? AND run_id=? AND staged_turn IS NULL",
        )
        .bind(message_id)
        .bind(run_id)
        .fetch_optional(&registry.pool)
        .await?
        .map(|row| {
            json!({
                "id":row.get::<String,_>("id"),
                "from":row.get::<String,_>("sender"),
                "to":row.get::<String,_>("recipient"),
                "body":row.get::<String,_>("body"),
                "reply_to":row.get::<Option<String>,_>("reply_to"),
                "created_at":row.get::<i64,_>("created_at"),
            })
        })
    } else {
        None
    };
    Ok(json!({
        "run_id": run_id,
        "state": state,
        "mode": "interactive",
        "version": version,
        "coordination_epoch": coordination_epoch,
        "cursor": cursor,
        "workers": workers,
        "message_count": message_count,
        "message_detail": message_detail,
        "turns": turns.iter().map(|r| json!({
            "id": r.get::<String,_>("id"),
            "agent_id": r.get::<String,_>("agent_id"),
            "state": r.get::<String,_>("state"),
            "started_at": r.get::<i64,_>("started_at"),
            "ended_at": r.get::<Option<i64>,_>("ended_at"),
        })).collect::<Vec<_>>(),
        "controller_inbox": inbox.iter().map(|r| json!({
            "id": r.get::<String,_>("id"),
            "from": r.get::<String,_>("sender"),
            "body_preview": r.get::<String,_>("body_preview"),
            "body_truncated": r.get::<i64,_>("body_truncated") != 0,
            "created_at": r.get::<i64,_>("created_at"),
            "accepted": r.get::<Option<i64>,_>("controller_ack_at").is_some(),
        })).collect::<Vec<_>>(),
        "messages": messages.iter().map(|r| json!({
            "id": r.get::<String,_>("id"),
            "from": r.get::<String,_>("sender"),
            "to": r.get::<String,_>("recipient"),
            "body_preview": r.get::<String,_>("body_preview"),
            "body_truncated": r.get::<i64,_>("body_truncated") != 0,
            "reply_to": r.get::<Option<String>,_>("reply_to"),
            "created_at": r.get::<i64,_>("created_at"),
        })).collect::<Vec<_>>(),
    }))
}

/// Read-only. Never acquires or renews the controller lease.
pub async fn status(registry: &Registry, token: &str, args: StatusArgs) -> Result<Value> {
    let principal = caller(registry, token).await?;
    let agent_id = principal
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("controller credential must belong to a managed lead"))?;
    if !valid_id(&args.run_id) {
        bail!("invalid run identity");
    }
    if args.timeout_seconds.unwrap_or(0) > 60
        || args
            .after_cursor
            .as_ref()
            .is_some_and(|cursor| cursor.len() > 128)
        || args
            .message_id
            .as_ref()
            .is_some_and(|message_id| !valid_id(message_id))
    {
        bail!("invalid status bounds");
    }
    let timeout = Duration::from_secs(args.timeout_seconds.unwrap_or(0));
    let deadline = Instant::now() + timeout;
    loop {
        let value = snapshot(
            registry,
            agent_id.as_str(),
            &args.run_id,
            args.message_id.as_deref(),
        )
        .await?;
        let changed =
            args.message_id.is_some() || args.after_cursor.as_deref() != value["cursor"].as_str();
        if changed || timeout.is_zero() || Instant::now() >= deadline {
            return Ok(value);
        }
        tokio::time::sleep(
            Duration::from_millis(200).min(deadline.saturating_duration_since(Instant::now())),
        )
        .await;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateAction {
    /// Send a bounded message from the controller to an exact engaged worker.
    Message {
        to: String,
        body: String,
        reply_to: Option<String>,
    },
    /// Acknowledge a worker report addressed to the controller inbox.
    AcceptReport { message_id: String },
    /// Finish a settled run with a final synthesis.
    Finish { result: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArgs {
    pub run_id: String,
    pub control_handle: String,
    pub connector_instance: String,
    pub expected_version: i64,
    pub expected_epoch: i64,
    pub idempotency_key: String,
    pub action: UpdateAction,
}

enum Gate {
    Replay(Value),
    Proceed { epoch: i64, lead_id: String },
}

/// Shared control-handle/connector/epoch/version fencing for every interactive
/// mutation. An exact idempotent retry short-circuits to the original receipt
/// before any lease or version check; a changed payload under the same key fails.
#[allow(clippy::too_many_arguments)]
async fn authorize_mutation(
    tx: &mut Transaction<'_, Sqlite>,
    principal_agent_id: &str,
    run_id: &str,
    control_handle: &str,
    connector_instance: &str,
    expected_version: i64,
    expected_epoch: i64,
    idempotency_key: &str,
    request: &str,
) -> Result<Gate> {
    if !valid_id(run_id)
        || !valid_handle(control_handle)
        || !valid_connector(connector_instance)
        || !valid_id(idempotency_key)
    {
        bail!("invalid mutation identifiers");
    }
    let run = sqlx::query("SELECT mode,lead_id,version,state FROM runs WHERE id=?")
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run not found"))?;
    if run.get::<String, _>("mode") != "interactive" {
        bail!("run is not an interactive run");
    }
    let lead_id: String = run.get("lead_id");
    if lead_id != principal_agent_id {
        bail!("controller credential does not own this run");
    }
    let prior = sqlx::query(
        "SELECT request,receipt FROM controller_mutations WHERE run_id=? AND idempotency_key=?",
    )
    .bind(run_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(prior) = prior {
        if prior.get::<String, _>("request") != request {
            bail!("idempotency key reused for a different mutation");
        }
        return Ok(Gate::Replay(serde_json::from_str(
            &prior.get::<String, _>("receipt"),
        )?));
    }
    let cap = sqlx::query("SELECT handle_hash,status FROM control_capabilities WHERE run_id=?")
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("control capability not found"))?;
    if cap.get::<String, _>("status") != "active"
        || cap.get::<String, _>("handle_hash") != handle_hash(control_handle)
    {
        bail!("invalid control handle");
    }
    if expected_version != run.get::<i64, _>("version") {
        bail!("stale run version");
    }
    let lease = sqlx::query(
        "SELECT connector_instance,epoch,expires_at FROM controller_leases WHERE run_id=?",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| anyhow::anyhow!("controller lease not found"))?;
    let current = now();
    let expired = lease.get::<i64, _>("expires_at") <= current;
    let epoch = if expired {
        lease.get::<i64, _>("epoch") + 1
    } else {
        if expected_epoch != lease.get::<i64, _>("epoch") {
            bail!("stale coordination epoch");
        }
        if lease.get::<String, _>("connector_instance") != connector_instance {
            bail!("run is controlled by another connector instance");
        }
        lease.get::<i64, _>("epoch")
    };
    sqlx::query(
        "UPDATE controller_leases SET connector_instance=?,epoch=?,expires_at=? WHERE run_id=?",
    )
    .bind(connector_instance)
    .bind(epoch)
    .bind(current + LEASE_TTL_SECONDS)
    .bind(run_id)
    .execute(&mut **tx)
    .await?;
    Ok(Gate::Proceed { epoch, lead_id })
}

#[allow(clippy::too_many_arguments)]
async fn finish_receipt(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    idempotency_key: &str,
    request: &str,
    mut receipt: Value,
    version: i64,
    epoch: i64,
) -> Result<Value> {
    receipt["version"] = json!(version);
    receipt["coordination_epoch"] = json!(epoch);
    sqlx::query("UPDATE runs SET version=? WHERE id=?")
        .bind(version)
        .bind(run_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO controller_mutations(run_id,idempotency_key,request,receipt,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(run_id)
    .bind(idempotency_key)
    .bind(request)
    .bind(receipt.to_string())
    .bind(now())
    .execute(&mut **tx)
    .await?;
    Ok(receipt)
}

pub async fn update(registry: &Registry, token: &str, args: UpdateArgs) -> Result<Value> {
    let principal = caller(registry, token).await?;
    let agent_id = principal
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("controller credential must belong to a managed lead"))?;
    let request = serde_json::to_string(&args.action)?;
    let mut tx = registry.pool.begin_with("BEGIN IMMEDIATE").await?;
    let gate = authorize_mutation(
        &mut tx,
        agent_id.as_str(),
        &args.run_id,
        &args.control_handle,
        &args.connector_instance,
        args.expected_version,
        args.expected_epoch,
        &args.idempotency_key,
        &request,
    )
    .await?;
    let (epoch, lead_id) = match gate {
        Gate::Replay(value) => {
            tx.commit().await?;
            return Ok(value);
        }
        Gate::Proceed { epoch, lead_id } => (epoch, lead_id),
    };
    let state: String = sqlx::query_scalar("SELECT state FROM runs WHERE id=?")
        .bind(&args.run_id)
        .fetch_one(&mut *tx)
        .await?;
    if matches!(
        state.as_str(),
        "completed" | "failed" | "cancelled" | "cancel_requested" | "interrupted" | "exhausted"
    ) {
        bail!("run is not accepting controller updates");
    }
    let receipt = match args.action {
        UpdateAction::Message { to, body, reply_to } => {
            if body.trim().is_empty() || body.len() > 8192 || to == lead_id {
                bail!("invalid message recipient or body");
            }
            let engaged: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM run_workers WHERE run_id=? AND agent_id=?",
            )
            .bind(&args.run_id)
            .bind(&to)
            .fetch_one(&mut *tx)
            .await?;
            if engaged != 1 {
                bail!("recipient is not an engaged worker in this run");
            }
            if let Some(reply) = &reply_to {
                let related: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM messages WHERE id=? AND run_id=? AND sender=? AND recipient=?",
                )
                .bind(reply)
                .bind(&args.run_id)
                .bind(&to)
                .bind(&lead_id)
                .fetch_one(&mut *tx)
                .await?;
                if related == 0 {
                    bail!("reply must address a report from that worker");
                }
            }
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE run_id=?")
                .bind(&args.run_id)
                .fetch_one(&mut *tx)
                .await?;
            let max_messages: i64 = sqlx::query_scalar("SELECT max_messages FROM runs WHERE id=?")
                .bind(&args.run_id)
                .fetch_one(&mut *tx)
                .await?;
            if count >= max_messages {
                bail!("message budget exhausted");
            }
            let message_id = new_id("msg");
            sqlx::query(
                "INSERT INTO messages(id,run_id,sender,recipient,body,reply_to,dedup_key,created_at) VALUES(?,?,'controller',?,?,?,?,?)",
            )
            .bind(&message_id)
            .bind(&args.run_id)
            .bind(&to)
            .bind(&body)
            .bind(&reply_to)
            .bind(&args.idempotency_key)
            .bind(now())
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE runs SET state='running',error=NULL WHERE id=? AND state='waiting_for_controller'",
            )
            .bind(&args.run_id)
            .execute(&mut *tx)
            .await?;
            json!({"status":"sent","message_id":message_id,"to":to})
        }
        UpdateAction::AcceptReport { message_id } => {
            let changed = sqlx::query(
                "UPDATE messages SET controller_ack_at=? WHERE id=? AND run_id=? AND recipient=? AND staged_turn IS NULL AND controller_ack_at IS NULL AND sender IN (SELECT agent_id FROM run_workers WHERE run_id=?) AND seq=(SELECT max(latest.seq) FROM messages latest WHERE latest.run_id=messages.run_id AND latest.sender=messages.sender AND latest.recipient=messages.recipient AND latest.staged_turn IS NULL)",
            )
            .bind(now())
            .bind(&message_id)
            .bind(&args.run_id)
            .bind(&lead_id)
            .bind(&args.run_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if changed != 1 {
                bail!("report not found or already accepted");
            }
            json!({"status":"accepted","message_id":message_id})
        }
        UpdateAction::Finish { result } => {
            if result.trim().is_empty() || result.len() > 16384 {
                bail!("invalid result");
            }
            let running: i64 =
                sqlx::query_scalar("SELECT count(*) FROM turns WHERE run_id=? AND state='running'")
                    .bind(&args.run_id)
                    .fetch_one(&mut *tx)
                    .await?;
            let pending_worker_messages: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM messages WHERE run_id=? AND delivered_turn IS NULL AND staged_turn IS NULL AND recipient!=?",
            )
            .bind(&args.run_id)
            .bind(&lead_id)
            .fetch_one(&mut *tx)
            .await?;
            let missing_worker_reports: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM run_workers rw WHERE rw.run_id=? AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.run_id=rw.run_id AND m.sender=rw.agent_id AND m.recipient=? AND m.staged_turn IS NULL AND m.controller_ack_at IS NOT NULL AND m.seq=(SELECT max(latest.seq) FROM messages latest WHERE latest.run_id=m.run_id AND latest.sender=m.sender AND latest.recipient=m.recipient AND latest.staged_turn IS NULL))",
            )
            .bind(&args.run_id)
            .bind(&lead_id)
            .fetch_one(&mut *tx)
            .await?;
            if running > 0 || pending_worker_messages > 0 || missing_worker_reports > 0 {
                bail!("run is not settled");
            }
            sqlx::query("UPDATE runs SET state='completed',result=? WHERE id=?")
                .bind(&result)
                .bind(&args.run_id)
                .execute(&mut *tx)
                .await?;
            json!({"status":"completed","acceptance":"requires_verification"})
        }
    };
    let current_version: i64 = sqlx::query_scalar("SELECT version FROM runs WHERE id=?")
        .bind(&args.run_id)
        .fetch_one(&mut *tx)
        .await?;
    let receipt = finish_receipt(
        &mut tx,
        &args.run_id,
        &args.idempotency_key,
        &request,
        receipt,
        current_version + 1,
        epoch,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelArgs {
    pub run_id: String,
    pub control_handle: String,
    pub connector_instance: String,
    pub expected_version: i64,
    pub expected_epoch: i64,
    pub idempotency_key: String,
    pub reason: Option<String>,
}

pub async fn cancel(registry: &Registry, token: &str, args: CancelArgs) -> Result<Value> {
    let principal = caller(registry, token).await?;
    let agent_id = principal
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("controller credential must belong to a managed lead"))?;
    let reason = args.reason.clone().unwrap_or_default();
    if reason.len() > 1024 {
        bail!("cancellation reason is too long");
    }
    let request = serde_json::to_string(&reason)?;
    let mut tx = registry.pool.begin_with("BEGIN IMMEDIATE").await?;
    let gate = authorize_mutation(
        &mut tx,
        agent_id.as_str(),
        &args.run_id,
        &args.control_handle,
        &args.connector_instance,
        args.expected_version,
        args.expected_epoch,
        &args.idempotency_key,
        &request,
    )
    .await?;
    let epoch = match gate {
        Gate::Replay(value) => {
            tx.commit().await?;
            return Ok(value);
        }
        Gate::Proceed { epoch, .. } => epoch,
    };
    let state: String = sqlx::query_scalar("SELECT state FROM runs WHERE id=?")
        .bind(&args.run_id)
        .fetch_one(&mut *tx)
        .await?;
    if matches!(state.as_str(), "completed" | "failed" | "cancelled") {
        bail!("run has already settled and cannot be cancelled");
    }
    let running: i64 =
        sqlx::query_scalar("SELECT count(*) FROM turns WHERE run_id=? AND state='running'")
            .bind(&args.run_id)
            .fetch_one(&mut *tx)
            .await?;
    let receipt = if running > 0 {
        sqlx::query("UPDATE runs SET state='cancel_requested',error=? WHERE id=?")
            .bind(format!(
                "cancellation requested by controller; native effect unconfirmed: {reason}"
            ))
            .bind(&args.run_id)
            .execute(&mut *tx)
            .await?;
        json!({"outcome":"stop_requested","native_effect":"unknown","run_state":"cancel_requested"})
    } else {
        sqlx::query("UPDATE runs SET state='cancelled',error=? WHERE id=?")
            .bind(format!("cancelled by controller: {reason}"))
            .bind(&args.run_id)
            .execute(&mut *tx)
            .await?;
        json!({"outcome":"confirmed","native_effect":"none","run_state":"cancelled"})
    };
    let current_version: i64 = sqlx::query_scalar("SELECT version FROM runs WHERE id=?")
        .bind(&args.run_id)
        .fetch_one(&mut *tx)
        .await?;
    let receipt = finish_receipt(
        &mut tx,
        &args.run_id,
        &args.idempotency_key,
        &request,
        receipt,
        current_version + 1,
        epoch,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}
