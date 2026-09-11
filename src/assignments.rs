//! First-class work and human decisions. Model operations are staged under a turn
//! lease; only the turn transaction publishes them. Admin functions are not MCP tools.
use crate::{
    registry::{Registry, RegistryError},
    teams::{new_id, now},
};
use anyhow::{Result, bail};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, Sqlite, Transaction};

pub const SCHEMA: &str = "
CREATE TABLE assignments (
 id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), creator TEXT NOT NULL REFERENCES agents(id), assignee TEXT NOT NULL REFERENCES agents(id), parent_id TEXT REFERENCES assignments(id),
 state TEXT NOT NULL CHECK(state IN ('open','accepted','in_progress','reported','closed','expired','cancelled')),
 objective TEXT NOT NULL, done_criteria TEXT NOT NULL, scope_hash TEXT NOT NULL, deadline INTEGER NOT NULL,
 turn_budget INTEGER NOT NULL CHECK(turn_budget>0), message_budget INTEGER NOT NULL CHECK(message_budget>0),
 turns_used INTEGER NOT NULL DEFAULT 0, messages_used INTEGER NOT NULL DEFAULT 0,
 created_at INTEGER NOT NULL, staged_turn TEXT REFERENCES turns(id));
CREATE INDEX assignments_run ON assignments(run_id);
CREATE TABLE decisions (
 id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), assignment_id TEXT REFERENCES assignments(id), requester TEXT NOT NULL REFERENCES agents(id),
 state TEXT NOT NULL CHECK(state IN ('requested','resolved','withdrawn','invalidated')),
 question TEXT NOT NULL, scope_hash TEXT NOT NULL, artifact_hash TEXT NOT NULL, options TEXT NOT NULL,
 blocking INTEGER NOT NULL, resolution TEXT, resolved_by TEXT, resolved_at INTEGER,
 created_at INTEGER NOT NULL, staged_turn TEXT REFERENCES turns(id));
CREATE TABLE work_operations (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL REFERENCES runs(id), agent_id TEXT NOT NULL REFERENCES agents(id), turn_id TEXT NOT NULL REFERENCES turns(id), dedup_key TEXT NOT NULL, request TEXT NOT NULL, receipt TEXT NOT NULL, kind TEXT NOT NULL, target TEXT NOT NULL, new_state TEXT, published INTEGER NOT NULL DEFAULT 0, UNIQUE(run_id,agent_id,dedup_key));
PRAGMA user_version=6;
";

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    pub run_id: String,
    pub assignee: String,
    pub parent_id: Option<String>,
    pub objective: String,
    pub done_criteria: Vec<String>,
    /// Immutable SHA-256 digest identifying the exact authorized work scope.
    pub scope_hash: String,
    /// Relative deadline from creation time. The runtime caps it at the run deadline.
    pub deadline_seconds: u32,
    pub turn_budget: u32,
    pub message_budget: u32,
    pub idempotency_key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArgs {
    pub run_id: String,
    pub assignment_id: String,
    /// Assignee: accepted, in_progress, reported. Lead: closed or cancelled.
    pub state: String,
    pub idempotency_key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionArgs {
    pub run_id: String,
    pub assignment_id: Option<String>,
    pub question: String,
    pub scope_hash: String,
    /// SHA-256 of the artifact or proposal the human is deciding on.
    pub artifact_hash: String,
    pub options: Vec<String>,
    pub blocking: bool,
    pub idempotency_key: String,
}
fn hash_valid(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn key_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) struct Actor<'a> {
    pub run: &'a str,
    pub agent: &'a str,
    pub turn: &'a str,
    pub lead: &'a str,
}

async fn replay(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor<'_>,
    key: &str,
    request: &str,
) -> Result<Option<Value>> {
    if !key_valid(key) {
        bail!("invalid work operation key");
    }
    let old =
        sqlx::query("SELECT * FROM work_operations WHERE run_id=? AND agent_id=? AND dedup_key=?")
            .bind(actor.run)
            .bind(actor.agent)
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some(old) = old {
        if old.get::<String, _>("request") != request {
            bail!("idempotency key reused for different work");
        }
        if old.get::<i64, _>("published") == 0 && old.get::<String, _>("turn_id") != actor.turn {
            bail!("prior uncommitted operation requires reconciliation");
        }
        return Ok(Some(serde_json::from_str(
            &old.get::<String, _>("receipt"),
        )?));
    }
    Ok(None)
}
#[allow(clippy::too_many_arguments)]
async fn record(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor<'_>,
    key: &str,
    request: &str,
    kind: &str,
    target: &str,
    state: Option<&str>,
    receipt: &Value,
) -> Result<()> {
    sqlx::query("INSERT INTO work_operations(run_id,agent_id,turn_id,dedup_key,request,receipt,kind,target,new_state) VALUES(?,?,?,?,?,?,?,?,?)").bind(actor.run).bind(actor.agent).bind(actor.turn).bind(key).bind(request).bind(receipt.to_string()).bind(kind).bind(target).bind(state).execute(&mut **tx).await?;
    Ok(())
}
async fn notify(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor<'_>,
    to: &str,
    id: &str,
    body: Value,
) -> Result<()> {
    charge_message(tx, actor.run, actor.agent).await?;
    let remaining:i64=sqlx::query_scalar("SELECT max_messages-(SELECT count(*) FROM messages WHERE run_id=runs.id) FROM runs WHERE id=?").bind(actor.run).fetch_one(&mut **tx).await?;
    if remaining <= 0 {
        bail!("message budget exhausted");
    }
    sqlx::query("INSERT INTO messages(id,run_id,sender,recipient,body,dedup_key,created_at,staged_turn) VALUES(?,?,?,?,?,?,?,?)").bind(new_id("msg")).bind(actor.run).bind(actor.agent).bind(to).bind(body.to_string()).bind(format!("work_{id}")).bind(now()).bind(actor.turn).execute(&mut **tx).await?;
    Ok(())
}

/// Charge all agent-produced messages, including assignment notifications, under
/// the same root budget. Unassigned agents cannot spend reserved worker slices.
pub(crate) async fn charge_message(
    tx: &mut Transaction<'_, Sqlite>,
    run: &str,
    agent: &str,
) -> Result<()> {
    let row=sqlx::query("SELECT id,message_budget,messages_used,deadline FROM assignments WHERE run_id=? AND assignee=? AND staged_turn IS NULL AND state NOT IN ('closed','expired','cancelled')").bind(run).bind(agent).fetch_optional(&mut **tx).await?;
    if let Some(row) = row {
        if row.get::<i64, _>("deadline") <= now()
            || row.get::<i64, _>("messages_used") >= row.get::<i64, _>("message_budget")
        {
            bail!("assignment message budget or deadline exhausted");
        }
        sqlx::query("UPDATE assignments SET messages_used=messages_used+1 WHERE id=?")
            .bind(row.get::<String, _>("id"))
            .execute(&mut **tx)
            .await?;
    } else {
        let free:i64=sqlx::query_scalar("SELECT max_messages-(SELECT count(*) FROM messages WHERE run_id=r.id)-COALESCE((SELECT sum(message_budget-messages_used) FROM assignments WHERE run_id=r.id AND state NOT IN ('closed','expired','cancelled')),0) FROM runs r WHERE id=?").bind(run).fetch_one(&mut **tx).await?;
        if free <= 0 {
            bail!("unreserved root message budget exhausted");
        }
    }
    Ok(())
}

pub(crate) async fn charge_turn(
    tx: &mut Transaction<'_, Sqlite>,
    run: &str,
    agent: &str,
) -> Result<bool> {
    let row=sqlx::query("SELECT id,turn_budget,turns_used,deadline FROM assignments WHERE run_id=? AND assignee=? AND staged_turn IS NULL AND state NOT IN ('closed','expired','cancelled')").bind(run).bind(agent).fetch_optional(&mut **tx).await?;
    let allowed = if let Some(row) = row {
        if row.get::<i64, _>("deadline") <= now()
            || row.get::<i64, _>("turns_used") >= row.get::<i64, _>("turn_budget")
        {
            false
        } else {
            sqlx::query("UPDATE assignments SET turns_used=turns_used+1 WHERE id=?")
                .bind(row.get::<String, _>("id"))
                .execute(&mut **tx)
                .await?;
            true
        }
    } else {
        let free:i64=sqlx::query_scalar("SELECT max_turns-turns-COALESCE((SELECT sum(turn_budget-turns_used) FROM assignments WHERE run_id=r.id AND state NOT IN ('closed','expired','cancelled')),0) FROM runs r WHERE id=?").bind(run).fetch_one(&mut **tx).await?;
        free > 0
    };
    if !allowed {
        sqlx::query("UPDATE runs SET state='stalled',error='assignment deadline or reserved turn budget requires reconciliation' WHERE id=?").bind(run).execute(&mut **tx).await?;
    }
    Ok(allowed)
}

pub(crate) async fn inbox_context(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor<'_>,
) -> Result<Value> {
    let assignments=sqlx::query("SELECT id,assignee,state,objective,scope_hash,deadline,turn_budget,turns_used,message_budget,messages_used FROM assignments WHERE run_id=? AND staged_turn IS NULL AND (assignee=? OR creator=?) ORDER BY created_at,id LIMIT 64").bind(actor.run).bind(actor.agent).bind(actor.agent).fetch_all(&mut **tx).await?;
    let decisions=sqlx::query("SELECT id,state,question,scope_hash,artifact_hash,resolution FROM decisions WHERE run_id=? AND staged_turn IS NULL AND (requester=? OR ?=?) ORDER BY created_at,id LIMIT 32").bind(actor.run).bind(actor.agent).bind(actor.agent).bind(actor.lead).fetch_all(&mut **tx).await?;
    Ok(
        json!({"assignments":assignments.iter().map(|r|json!({"id":r.get::<String,_>("id"),"assignee":r.get::<String,_>("assignee"),"state":r.get::<String,_>("state"),"objective":r.get::<String,_>("objective"),"scope_hash":r.get::<String,_>("scope_hash"),"deadline":r.get::<i64,_>("deadline"),"remaining_turns":r.get::<i64,_>("turn_budget")-r.get::<i64,_>("turns_used"),"remaining_messages":r.get::<i64,_>("message_budget")-r.get::<i64,_>("messages_used")})).collect::<Vec<_>>(),"decisions":decisions.iter().map(|r|json!({"id":r.get::<String,_>("id"),"state":r.get::<String,_>("state"),"question":r.get::<String,_>("question"),"scope_hash":r.get::<String,_>("scope_hash"),"artifact_hash":r.get::<String,_>("artifact_hash"),"resolution":r.get::<Option<String>,_>("resolution")})).collect::<Vec<_>>()}),
    )
}

pub(crate) async fn inspect_run(
    registry: &Registry,
    run: &str,
) -> std::result::Result<Value, RegistryError> {
    let assignments=sqlx::query("SELECT id,creator,assignee,parent_id,state,objective,done_criteria,scope_hash,deadline,turn_budget,message_budget,turns_used,messages_used,created_at FROM assignments WHERE run_id=? AND staged_turn IS NULL ORDER BY created_at,id LIMIT 128").bind(run).fetch_all(&registry.pool).await?;
    let decisions=sqlx::query("SELECT id,assignment_id,requester,state,question,scope_hash,artifact_hash,options,blocking,resolution,resolved_by,resolved_at,created_at FROM decisions WHERE run_id=? AND staged_turn IS NULL ORDER BY created_at,id LIMIT 128").bind(run).fetch_all(&registry.pool).await?;
    Ok(json!({
        "assignments":assignments.iter().map(|r|json!({"id":r.get::<String,_>("id"),"creator":r.get::<String,_>("creator"),"assignee":r.get::<String,_>("assignee"),"parent_id":r.get::<Option<String>,_>("parent_id"),"state":r.get::<String,_>("state"),"objective":r.get::<String,_>("objective"),"done_criteria":serde_json::from_str::<Value>(&r.get::<String,_>("done_criteria")).unwrap_or(Value::Null),"scope_hash":r.get::<String,_>("scope_hash"),"deadline":r.get::<i64,_>("deadline"),"turn_budget":r.get::<i64,_>("turn_budget"),"message_budget":r.get::<i64,_>("message_budget"),"turns_used":r.get::<i64,_>("turns_used"),"messages_used":r.get::<i64,_>("messages_used"),"created_at":r.get::<i64,_>("created_at")})).collect::<Vec<_>>(),
        "decisions":decisions.iter().map(|r|json!({"id":r.get::<String,_>("id"),"assignment_id":r.get::<Option<String>,_>("assignment_id"),"requester":r.get::<String,_>("requester"),"state":r.get::<String,_>("state"),"question":r.get::<String,_>("question"),"scope_hash":r.get::<String,_>("scope_hash"),"artifact_hash":r.get::<String,_>("artifact_hash"),"options":serde_json::from_str::<Value>(&r.get::<String,_>("options")).unwrap_or(Value::Null),"blocking":r.get::<bool,_>("blocking"),"resolution":r.get::<Option<String>,_>("resolution"),"resolved_by":r.get::<Option<String>,_>("resolved_by"),"resolved_at":r.get::<Option<i64>,_>("resolved_at"),"created_at":r.get::<i64,_>("created_at")})).collect::<Vec<_>>()
    }))
}

pub(crate) async fn create(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor<'_>,
    args: CreateArgs,
) -> Result<Value> {
    let request = serde_json::to_string(&args)?;
    if let Some(value) = replay(tx, actor, &args.idempotency_key, &request).await? {
        return Ok(value);
    }
    if actor.agent != actor.lead {
        bail!("only the lead can allocate work");
    }
    if args.assignee == actor.agent
        || args.objective.trim().is_empty()
        || args.objective.len() > 4096
        || args.done_criteria.is_empty()
        || args.done_criteria.len() > 16
        || args
            .done_criteria
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 512)
        || !hash_valid(&args.scope_hash)
        || !(30..=3600).contains(&args.deadline_seconds)
        || args.turn_budget == 0
        || args.message_budget == 0
    {
        bail!("invalid assignment scope or limits");
    }
    let run = sqlx::query("SELECT * FROM runs WHERE id=?")
        .bind(actor.run)
        .fetch_one(&mut **tx)
        .await?;
    let member:i64=sqlx::query_scalar("SELECT count(*) FROM agents a JOIN team_members m ON m.agent_id=a.id WHERE a.id=? AND a.team_id=?").bind(&args.assignee).bind(run.get::<String,_>("team_id")).fetch_one(&mut **tx).await?;
    if member != 1 {
        bail!("invalid assignee");
    }
    let deadline = (now() + i64::from(args.deadline_seconds)).min(run.get("deadline"));
    // One current scope per agent makes charging each native turn/message unambiguous.
    let occupied:i64=sqlx::query_scalar("SELECT count(*) FROM assignments WHERE run_id=? AND assignee=? AND state NOT IN ('closed','expired','cancelled')").bind(actor.run).bind(&args.assignee).fetch_one(&mut **tx).await?;
    if occupied > 0 {
        bail!("assignee already has an unfinished assignment");
    }
    if let Some(parent) = &args.parent_id {
        let valid:i64=sqlx::query_scalar("SELECT count(*) FROM assignments WHERE id=? AND run_id=? AND (staged_turn IS NULL OR staged_turn=?) AND state NOT IN ('closed','expired','cancelled') AND scope_hash=? AND deadline>=? AND turn_budget>=? AND message_budget>=?").bind(parent).bind(actor.run).bind(actor.turn).bind(&args.scope_hash).bind(deadline).bind(i64::from(args.turn_budget)).bind(i64::from(args.message_budget)).fetch_one(&mut **tx).await?;
        if valid != 1 {
            bail!("parent assignment not available in this run");
        }
    }
    let reserved=sqlx::query("SELECT COALESCE(sum(turn_budget-turns_used),0) AS turns,COALESCE(sum(message_budget-messages_used),0) AS messages FROM assignments WHERE run_id=? AND state NOT IN ('closed','expired','cancelled')").bind(actor.run).fetch_one(&mut **tx).await?;
    let sent: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE run_id=?")
        .bind(actor.run)
        .fetch_one(&mut **tx)
        .await?;
    // Keep one unreserved turn for the lead to integrate reports.
    if reserved.get::<i64, _>("turns") + i64::from(args.turn_budget) + 1
        > run.get::<i64, _>("max_turns") - run.get::<i64, _>("turns")
        || reserved.get::<i64, _>("messages") + i64::from(args.message_budget) + 1
            > run.get::<i64, _>("max_messages") - sent
    {
        bail!("assignment exceeds remaining unreserved root budget");
    }
    let id = new_id("assignment");
    sqlx::query("INSERT INTO assignments(id,run_id,creator,assignee,parent_id,state,objective,done_criteria,scope_hash,deadline,turn_budget,message_budget,created_at,staged_turn) VALUES(?,?,?,?,?,'open',?,?,?,?,?,?,?,?)").bind(&id).bind(actor.run).bind(actor.agent).bind(&args.assignee).bind(&args.parent_id).bind(&args.objective).bind(serde_json::to_string(&args.done_criteria)?).bind(&args.scope_hash).bind(deadline).bind(i64::from(args.turn_budget)).bind(i64::from(args.message_budget)).bind(now()).bind(actor.turn).execute(&mut **tx).await?;
    notify(tx,actor,&args.assignee,&id,json!({"kind":"assignment","assignment_id":id,"objective":args.objective,"done_criteria":args.done_criteria,"scope_hash":args.scope_hash,"deadline":deadline,"turn_budget":args.turn_budget,"message_budget":args.message_budget})).await?;
    let receipt = json!({"status":"staged","assignment_id":id,"deadline":deadline});
    record(
        tx,
        actor,
        &args.idempotency_key,
        &request,
        "assignment_create",
        &id,
        None,
        &receipt,
    )
    .await?;
    Ok(receipt)
}

pub(crate) async fn update(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor<'_>,
    args: UpdateArgs,
) -> Result<Value> {
    let request = serde_json::to_string(&args)?;
    if let Some(value) = replay(tx, actor, &args.idempotency_key, &request).await? {
        return Ok(value);
    }
    let row=sqlx::query("SELECT * FROM assignments WHERE id=? AND run_id=? AND (staged_turn IS NULL OR staged_turn=?)").bind(&args.assignment_id).bind(actor.run).bind(actor.turn).fetch_optional(&mut **tx).await?.ok_or_else(||anyhow::anyhow!("assignment not found"))?;
    let staged:Option<String>=sqlx::query_scalar("SELECT new_state FROM work_operations WHERE turn_id=? AND target=? AND kind='assignment_update' ORDER BY id DESC LIMIT 1").bind(actor.turn).bind(&args.assignment_id).fetch_optional(&mut **tx).await?;
    let old = staged.unwrap_or_else(|| row.get("state"));
    let allowed = match args.state.as_str() {
        "accepted" => actor.agent == row.get::<String, _>("assignee") && old == "open",
        "in_progress" => {
            actor.agent == row.get::<String, _>("assignee")
                && ["open", "accepted"].contains(&old.as_str())
        }
        "reported" => {
            actor.agent == row.get::<String, _>("assignee")
                && ["open", "accepted", "in_progress"].contains(&old.as_str())
        }
        "closed" => actor.agent == actor.lead && old == "reported",
        "cancelled" => {
            actor.agent == actor.lead && !["closed", "expired", "cancelled"].contains(&old.as_str())
        }
        _ => false,
    };
    if !allowed {
        bail!("assignment transition not authorized");
    }
    if ["closed", "cancelled"].contains(&args.state.as_str()) {
        let children:i64=sqlx::query_scalar("SELECT count(*) FROM assignments a WHERE parent_id=? AND COALESCE((SELECT new_state FROM work_operations WHERE turn_id=? AND target=a.id AND kind='assignment_update' ORDER BY id DESC LIMIT 1),state) NOT IN ('closed','expired','cancelled')").bind(&args.assignment_id).bind(actor.turn).fetch_one(&mut **tx).await?;
        if children > 0 {
            bail!("close or cancel child assignments first");
        }
    }
    if args.state == "reported" {
        notify(
            tx,
            actor,
            actor.lead,
            &new_id("report"),
            json!({"kind":"assignment_reported","assignment_id":args.assignment_id}),
        )
        .await?;
    }
    let receipt = json!({"status":"staged","assignment_id":args.assignment_id,"state":args.state});
    record(
        tx,
        actor,
        &args.idempotency_key,
        &request,
        "assignment_update",
        &args.assignment_id,
        Some(&args.state),
        &receipt,
    )
    .await?;
    Ok(receipt)
}

pub(crate) async fn request_decision(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor<'_>,
    args: DecisionArgs,
) -> Result<Value> {
    let request = serde_json::to_string(&args)?;
    if let Some(value) = replay(tx, actor, &args.idempotency_key, &request).await? {
        return Ok(value);
    }
    if args.question.trim().is_empty()
        || args.question.len() > 4096
        || !hash_valid(&args.scope_hash)
        || !hash_valid(&args.artifact_hash)
        || args.options.len() < 2
        || args.options.len() > 8
        || args
            .options
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 256)
        || args
            .options
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != args.options.len()
    {
        bail!("invalid concrete decision scope");
    }
    if let Some(id) = &args.assignment_id {
        let row=sqlx::query("SELECT * FROM assignments WHERE id=? AND run_id=? AND (staged_turn IS NULL OR staged_turn=?)").bind(id).bind(actor.run).bind(actor.turn).fetch_optional(&mut **tx).await?.ok_or_else(||anyhow::anyhow!("assignment not found"))?;
        if (actor.agent != actor.lead && actor.agent != row.get::<String, _>("assignee"))
            || args.scope_hash != row.get::<String, _>("scope_hash")
            || ["closed", "expired", "cancelled"].contains(&row.get::<String, _>("state").as_str())
        {
            bail!("decision scope is not current or authorized");
        }
    } else if actor.agent != actor.lead {
        bail!("worker decision requires its assignment");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM decisions WHERE run_id=?")
        .bind(actor.run)
        .fetch_one(&mut **tx)
        .await?;
    if count >= 32 {
        bail!("decision limit exhausted");
    }
    let id = new_id("decision");
    sqlx::query("INSERT INTO decisions(id,run_id,assignment_id,requester,state,question,scope_hash,artifact_hash,options,blocking,created_at,staged_turn) VALUES(?,?,?,?,'requested',?,?,?,?,?,?,?)").bind(&id).bind(actor.run).bind(&args.assignment_id).bind(actor.agent).bind(&args.question).bind(&args.scope_hash).bind(&args.artifact_hash).bind(serde_json::to_string(&args.options)?).bind(args.blocking).bind(now()).bind(actor.turn).execute(&mut **tx).await?;
    let receipt = json!({"status":"staged","decision_id":id,"approval_granted":false});
    record(
        tx,
        actor,
        &args.idempotency_key,
        &request,
        "decision_request",
        &id,
        None,
        &receipt,
    )
    .await?;
    Ok(receipt)
}

pub(crate) async fn publish(tx: &mut Transaction<'_, Sqlite>, turn: &str) -> Result<()> {
    let updates=sqlx::query("SELECT target,new_state FROM work_operations WHERE turn_id=? AND published=0 AND kind='assignment_update' ORDER BY id").bind(turn).fetch_all(&mut **tx).await?;
    for update in updates {
        let id: String = update.get("target");
        let state: String = update.get("new_state");
        sqlx::query("UPDATE assignments SET state=? WHERE id=?")
            .bind(&state)
            .bind(&id)
            .execute(&mut **tx)
            .await?;
        if ["closed", "expired", "cancelled"].contains(&state.as_str()) {
            sqlx::query("UPDATE decisions SET state='invalidated' WHERE assignment_id=? AND state IN ('requested','resolved')").bind(&id).execute(&mut **tx).await?;
        }
    }
    sqlx::query("UPDATE assignments SET staged_turn=NULL WHERE staged_turn=?")
        .bind(turn)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE decisions SET staged_turn=NULL WHERE staged_turn=?")
        .bind(turn)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE work_operations SET published=1 WHERE turn_id=?")
        .bind(turn)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Discard internal operations that never reached `turn_commit`. None of these
/// rows were visible to recipients, so removal is reconciliation rather than a
/// replay of an uncertain external effect.
pub(crate) async fn discard(tx: &mut Transaction<'_, Sqlite>, turn: &str) -> Result<()> {
    sqlx::query("UPDATE assignments SET messages_used=MAX(0,messages_used-(SELECT count(*) FROM messages m WHERE m.staged_turn=? AND m.run_id=assignments.run_id AND m.sender=assignments.assignee)) WHERE run_id=(SELECT run_id FROM turns WHERE id=?)")
        .bind(turn)
        .bind(turn)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM decisions WHERE staged_turn=?")
        .bind(turn)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM work_operations WHERE turn_id=? AND published=0")
        .bind(turn)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM assignments WHERE staged_turn=?")
        .bind(turn)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Trusted administrator entry point. Caller authenticates the human before calling;
/// no model credential or model-supplied claim of approval is accepted here.
pub async fn resolve_decision(
    registry: &Registry,
    id: &str,
    scope_hash: &str,
    artifact_hash: &str,
    choice: &str,
    human: &str,
) -> Result<Value> {
    if human.trim().is_empty() || human.len() > 256 {
        bail!("authenticated human identity required");
    }
    let mut tx = registry.pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT * FROM decisions WHERE id=? AND staged_turn IS NULL")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("decision not found"))?;
    if row.get::<String, _>("state") != "requested" {
        bail!("decision is no longer requested");
    }
    if scope_hash != row.get::<String, _>("scope_hash")
        || artifact_hash != row.get::<String, _>("artifact_hash")
    {
        bail!("decision revision mismatch");
    }
    let choices: Vec<String> = serde_json::from_str(&row.get::<String, _>("options"))?;
    if !choices.iter().any(|s| s == choice) {
        bail!("choice not offered");
    }
    if let Some(assignment) = row.get::<Option<String>, _>("assignment_id") {
        let current:i64=sqlx::query_scalar("SELECT count(*) FROM assignments WHERE id=? AND scope_hash=? AND deadline>? AND state NOT IN ('closed','expired','cancelled')").bind(assignment).bind(scope_hash).bind(now()).fetch_one(&mut *tx).await?;
        if current != 1 {
            sqlx::query("UPDATE decisions SET state='invalidated' WHERE id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            bail!("decision assignment scope is stale; request invalidated");
        }
    }
    sqlx::query("UPDATE decisions SET state='resolved',resolution=?,resolved_by=?,resolved_at=? WHERE id=? AND state='requested'").bind(choice).bind(human).bind(now()).bind(id).execute(&mut *tx).await?;
    // Resolution is durable even if the run no longer has delivery capacity.
    // A human response is a broker-authored event, never a model-granted capability.
    let run: String = row.get("run_id");
    let capacity:i64=sqlx::query_scalar("SELECT max_messages-(SELECT count(*) FROM messages WHERE run_id=r.id) FROM runs r WHERE id=? AND state IN ('queued','running','stalled') AND deadline>?").bind(&run).bind(now()).fetch_optional(&mut *tx).await?.unwrap_or(0);
    if capacity > 0 {
        sqlx::query("INSERT INTO messages(id,run_id,sender,recipient,body,dedup_key,created_at) VALUES(?,?,'human',?,?,?,?)").bind(new_id("msg")).bind(&run).bind(row.get::<String,_>("requester")).bind(json!({"kind":"decision_resolved","decision_id":id,"choice":choice,"scope_hash":scope_hash,"artifact_hash":artifact_hash}).to_string()).bind(format!("resolution_{id}")).bind(now()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(
        json!({"decision_id":id,"state":"resolved","choice":choice,"scope_hash":scope_hash,"artifact_hash":artifact_hash}),
    )
}

/// Explicit administrative invalidation preserves the original resolution receipt.
pub async fn invalidate_decision(registry: &Registry, id: &str) -> Result<()> {
    let changed=sqlx::query("UPDATE decisions SET state='invalidated' WHERE id=? AND staged_turn IS NULL AND state IN ('requested','resolved')").bind(id).execute(&registry.pool).await?.rows_affected();
    if changed != 1 {
        bail!("decision not available for invalidation");
    }
    Ok(())
}

pub async fn inspect_decision(registry: &Registry, id: &str) -> Result<Value> {
    let row = sqlx::query("SELECT id,run_id,assignment_id,requester,state,question,scope_hash,artifact_hash,options,blocking,resolution,resolved_by,resolved_at,created_at FROM decisions WHERE id=? AND staged_turn IS NULL")
        .bind(id)
        .fetch_optional(&registry.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("decision not found"))?;
    Ok(
        json!({"id":row.get::<String,_>("id"),"run_id":row.get::<String,_>("run_id"),"assignment_id":row.get::<Option<String>,_>("assignment_id"),"requester":row.get::<String,_>("requester"),"state":row.get::<String,_>("state"),"question":row.get::<String,_>("question"),"scope_hash":row.get::<String,_>("scope_hash"),"artifact_hash":row.get::<String,_>("artifact_hash"),"options":serde_json::from_str::<Value>(&row.get::<String,_>("options"))?,"blocking":row.get::<bool,_>("blocking"),"resolution":row.get::<Option<String>,_>("resolution"),"resolved_by":row.get::<Option<String>,_>("resolved_by"),"resolved_at":row.get::<Option<i64>,_>("resolved_at"),"created_at":row.get::<i64,_>("created_at")}),
    )
}

/// Inspect one published assignment through the trusted local administration surface.
pub async fn inspect_assignment(registry: &Registry, id: &str) -> Result<Value> {
    let row = sqlx::query("SELECT id,run_id,creator,assignee,parent_id,state,objective,done_criteria,scope_hash,deadline,turn_budget,message_budget,turns_used,messages_used,created_at FROM assignments WHERE id=? AND staged_turn IS NULL")
        .bind(id)
        .fetch_optional(&registry.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("assignment not found"))?;
    Ok(json!({
        "id":row.get::<String,_>("id"),
        "run_id":row.get::<String,_>("run_id"),
        "creator":row.get::<String,_>("creator"),
        "assignee":row.get::<String,_>("assignee"),
        "parent_id":row.get::<Option<String>,_>("parent_id"),
        "state":row.get::<String,_>("state"),
        "objective":row.get::<String,_>("objective"),
        "done_criteria":serde_json::from_str::<Value>(&row.get::<String,_>("done_criteria"))?,
        "scope_hash":row.get::<String,_>("scope_hash"),
        "deadline":row.get::<i64,_>("deadline"),
        "turn_budget":row.get::<i64,_>("turn_budget"),
        "message_budget":row.get::<i64,_>("message_budget"),
        "turns_used":row.get::<i64,_>("turns_used"),
        "messages_used":row.get::<i64,_>("messages_used"),
        "created_at":row.get::<i64,_>("created_at")
    }))
}

/// Extend a stuck assignment only from capacity that remains inside the run's
/// original root limits. The caller must hold the local administration lock.
pub async fn extend_assignment(
    registry: &Registry,
    id: &str,
    add_turns: u32,
    add_messages: u32,
    deadline_seconds: u32,
) -> Result<Value> {
    if (add_turns == 0 && add_messages == 0 && deadline_seconds == 0)
        || add_turns > 64
        || add_messages > 256
        || (deadline_seconds != 0 && !(30..=3600).contains(&deadline_seconds))
    {
        bail!("assignment extension is outside the bounded range");
    }
    let mut tx = registry.pool.begin_with("BEGIN IMMEDIATE").await?;
    let assignment = sqlx::query("SELECT * FROM assignments WHERE id=? AND staged_turn IS NULL AND state NOT IN ('closed','expired','cancelled')")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("active assignment not found"))?;
    let run_id: String = assignment.get("run_id");
    let run = sqlx::query("SELECT * FROM runs WHERE id=?")
        .bind(&run_id)
        .fetch_one(&mut *tx)
        .await?;
    if !["queued", "running", "stalled"].contains(&run.get::<String, _>("state").as_str())
        || run.get::<i64, _>("deadline") <= now()
    {
        bail!("assignment run no longer accepts bounded recovery");
    }
    let reserved=sqlx::query("SELECT COALESCE(sum(turn_budget-turns_used),0) AS turns,COALESCE(sum(message_budget-messages_used),0) AS messages FROM assignments WHERE run_id=? AND staged_turn IS NULL AND state NOT IN ('closed','expired','cancelled')").bind(&run_id).fetch_one(&mut *tx).await?;
    let sent: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE run_id=?")
        .bind(&run_id)
        .fetch_one(&mut *tx)
        .await?;
    // Retain one unreserved integration turn and message for the lead, exactly
    // as assignment creation does. This operation never enlarges root limits.
    if reserved.get::<i64, _>("turns") + i64::from(add_turns) + 1
        > run.get::<i64, _>("max_turns") - run.get::<i64, _>("turns")
        || reserved.get::<i64, _>("messages") + i64::from(add_messages) + 1
            > run.get::<i64, _>("max_messages") - sent
    {
        bail!("assignment extension exceeds remaining unreserved root budget");
    }
    let old_deadline: i64 = assignment.get("deadline");
    let deadline = if deadline_seconds == 0 {
        old_deadline
    } else {
        old_deadline.max((now() + i64::from(deadline_seconds)).min(run.get::<i64, _>("deadline")))
    };
    sqlx::query("UPDATE assignments SET turn_budget=turn_budget+?,message_budget=message_budget+?,deadline=? WHERE id=?")
        .bind(i64::from(add_turns))
        .bind(i64::from(add_messages))
        .bind(deadline)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(json!({
        "assignment_id":id,
        "run_id":run_id,
        "state":assignment.get::<String,_>("state"),
        "turn_budget":assignment.get::<i64,_>("turn_budget") + i64::from(add_turns),
        "message_budget":assignment.get::<i64,_>("message_budget") + i64::from(add_messages),
        "deadline":deadline,
        "root_limits":"unchanged",
        "run_resume_required":run.get::<String,_>("state") == "stalled"
    }))
}
