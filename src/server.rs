use crate::{
    model::Query,
    registry::{Registry, RegistryError},
    teams::ControllerStartArgs,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    future::IntoFuture,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use tokio::net::TcpListener;

const SCHEDULER_INSPECTION_ONLY: u8 = 0;
const SCHEDULER_RUNNING: u8 = 1;
const SCHEDULER_FAILED: u8 = 2;

/// Process-local scheduler state. The database is still the durable source of
/// record; this fence prevents an HTTP process from claiming that a scheduler
/// which has failed in this process is authoritative.
#[derive(Clone)]
struct SchedulerHealth(Arc<AtomicU8>);

impl SchedulerHealth {
    fn inspection_only() -> Self {
        Self(Arc::new(AtomicU8::new(SCHEDULER_INSPECTION_ONLY)))
    }

    fn running(&self) {
        self.0.store(SCHEDULER_RUNNING, Ordering::Release);
    }

    fn failed(&self) {
        self.0.store(SCHEDULER_FAILED, Ordering::Release);
    }

    fn is_failed(&self) -> bool {
        self.0.load(Ordering::Acquire) == SCHEDULER_FAILED
    }

    fn is_running(&self) -> bool {
        self.0.load(Ordering::Acquire) == SCHEDULER_RUNNING
    }

    fn json(&self) -> Value {
        match self.0.load(Ordering::Acquire) {
            SCHEDULER_INSPECTION_ONLY => {
                json!({"state":"inspection_only","authoritative":true})
            }
            SCHEDULER_RUNNING => json!({"state":"running","authoritative":true}),
            SCHEDULER_FAILED => json!({"state":"failed","authoritative":false}),
            _ => json!({"state":"unknown","authoritative":false}),
        }
    }
}

#[derive(Clone)]
struct AppState {
    registry: Registry,
    scheduler: SchedulerHealth,
}

fn public_error(error: RegistryError) -> (StatusCode, Json<Value>) {
    match error {
        RegistryError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        ),
        RegistryError::NotFound => (StatusCode::NOT_FOUND, Json(json!({"error":"not_found"}))),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"registry_unavailable"})),
        ),
    }
}

fn bearer_token(headers: &HeaderMap) -> Result<Option<&str>, (StatusCode, Json<Value>)> {
    let values: Vec<_> = headers.get_all("authorization").iter().collect();
    match values.as_slice() {
        [] => Ok(None),
        [header] => {
            let raw = header
                .to_str()
                .map_err(|_| public_error(RegistryError::Unauthorized))?;
            Ok(Some(
                raw.strip_prefix("Bearer ")
                    .filter(|token| !token.is_empty())
                    .ok_or_else(|| public_error(RegistryError::Unauthorized))?,
            ))
        }
        _ => Err(public_error(RegistryError::Unauthorized)),
    }
}

async fn inspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(query): Json<Query>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // This local endpoint has no browser use case. Reject Origin to reduce accidental
    // exposure to websites; the credential is still required for all record access.
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
        ));
    }
    let token = bearer_token(&headers)?;
    let is_run_inspection = matches!(&query, Query::RunsInspect { .. });
    let mut value = state
        .registry
        .inspect(token, query)
        .await
        .map_err(public_error)?;
    if is_run_inspection {
        let scheduler = state.scheduler.json();
        let authoritative = scheduler["authoritative"].as_bool().unwrap_or(false);
        if !value["activity"].is_object() {
            value["activity"] = json!({});
        }
        value["activity"]["scheduler"] = scheduler;
        value["activity"]["authoritative"] = json!(authoritative);
    }
    Ok(Json(value))
}

fn public_action_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    if error
        .downcast_ref::<RegistryError>()
        .is_some_and(|value| matches!(value, RegistryError::Unauthorized))
    {
        return public_error(RegistryError::Unauthorized);
    }
    if error
        .downcast_ref::<RegistryError>()
        .is_some_and(|value| matches!(value, RegistryError::NotFound))
    {
        return public_error(RegistryError::NotFound);
    }

    // `teams::act` has historical string errors. Keep those implementation
    // details off the network while retaining a small, stable public contract.
    let detail = error.to_string();
    let (status, code) = if detail.contains("run not found") {
        (StatusCode::NOT_FOUND, "not_found")
    } else if detail.contains("exact current turn lease")
        || detail.contains("turn lease is fenced")
        || detail.contains("turn lease is already committed")
    {
        (StatusCode::FORBIDDEN, "lease_not_active")
    } else if detail.contains("no owned active turn")
        || detail.contains("agent binding is required")
        || detail.contains("only the lead")
    {
        (StatusCode::FORBIDDEN, "action_not_permitted")
    } else if detail.contains("messages are still pending")
        || detail.contains("teammates must settle")
    {
        (StatusCode::CONFLICT, "work_pending")
    } else if detail.contains("message budget exhausted") {
        (StatusCode::CONFLICT, "budget_exhausted")
    } else if detail.contains("idempotency key reused") {
        (StatusCode::CONFLICT, "idempotency_conflict")
    } else if detail.contains("team already has an active run") {
        (StatusCode::CONFLICT, "run_already_active")
    } else if detail.contains("reply must address the original sender") {
        (StatusCode::BAD_REQUEST, "invalid_reply")
    } else if detail.contains("database") || detail.contains("SQL") {
        (StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable")
    } else {
        (StatusCode::BAD_REQUEST, "action_rejected")
    };
    (status, Json(json!({"error":code})))
}

async fn team_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(action): Json<crate::teams::Action>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
        ));
    }
    if state.scheduler.is_failed() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"scheduler_unavailable"})),
        ));
    }
    let token = bearer_token(&headers)?.ok_or_else(|| public_error(RegistryError::Unauthorized))?;
    crate::teams::act(&state.registry, token, action)
        .await
        .map(Json)
        .map_err(public_action_error)
}

fn public_start_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    if error
        .downcast_ref::<RegistryError>()
        .is_some_and(|value| matches!(value, RegistryError::Unauthorized))
    {
        return public_error(RegistryError::Unauthorized);
    }
    let detail = error.to_string();
    let (status, code) = if detail.contains("controller credential must belong") {
        (StatusCode::FORBIDDEN, "controller_forbidden")
    } else if detail.contains("idempotency key reused") {
        (StatusCode::CONFLICT, "idempotency_conflict")
    } else if detail.contains("team already has an active run") {
        (StatusCode::CONFLICT, "run_already_active")
    } else if detail.contains("live authorization") {
        (StatusCode::BAD_REQUEST, "live_authorization_required")
    } else if detail.contains("invalid objective") || detail.contains("invalid idempotency") {
        (StatusCode::BAD_REQUEST, "invalid_request")
    } else if detail.contains("database") || detail.contains("SQL") {
        (StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable")
    } else {
        (StatusCode::BAD_REQUEST, "start_rejected")
    };
    (status, Json(json!({"error":code})))
}

async fn controller_run_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<ControllerStartArgs>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
        ));
    }
    if !state.scheduler.is_running() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"scheduler_unavailable"})),
        ));
    }
    let token = bearer_token(&headers)?.ok_or_else(|| public_error(RegistryError::Unauthorized))?;
    crate::teams::start_as_controller(&state.registry, token, args)
        .await
        .map(Json)
        .map_err(public_start_error)
}

fn public_interactive_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    if error
        .downcast_ref::<RegistryError>()
        .is_some_and(|value| matches!(value, RegistryError::Unauthorized))
    {
        return public_error(RegistryError::Unauthorized);
    }
    let detail = error.to_string();
    let (status, code) = if detail.contains("run not found") {
        (StatusCode::NOT_FOUND, "not_found")
    } else if detail.contains("controller credential must belong")
        || detail.contains("controller credential does not own")
    {
        (StatusCode::FORBIDDEN, "controller_forbidden")
    } else if detail.contains("invalid control handle") {
        (StatusCode::FORBIDDEN, "invalid_control_handle")
    } else if detail.contains("another connector instance") {
        (StatusCode::CONFLICT, "connector_fenced")
    } else if detail.contains("stale coordination epoch") {
        (StatusCode::CONFLICT, "stale_epoch")
    } else if detail.contains("stale run version") {
        (StatusCode::CONFLICT, "stale_version")
    } else if detail.contains("idempotency key reused") {
        (StatusCode::CONFLICT, "idempotency_conflict")
    } else if detail.contains("team already has an active run") {
        (StatusCode::CONFLICT, "run_already_active")
    } else if detail.contains("live authorization") {
        (StatusCode::BAD_REQUEST, "live_authorization_required")
    } else if detail.contains("not settled") {
        (StatusCode::CONFLICT, "work_pending")
    } else if detail.contains("already settled") {
        (StatusCode::CONFLICT, "run_already_settled")
    } else if detail.contains("database") || detail.contains("SQL") {
        (StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable")
    } else {
        (StatusCode::BAD_REQUEST, "request_rejected")
    };
    (status, Json(json!({"error":code})))
}

async fn interactive_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<crate::interactive::StartArgs>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
        ));
    }
    if !state.scheduler.is_running() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"scheduler_unavailable"})),
        ));
    }
    let token = bearer_token(&headers)?.ok_or_else(|| public_error(RegistryError::Unauthorized))?;
    crate::interactive::start(&state.registry, token, args)
        .await
        .map(Json)
        .map_err(public_interactive_error)
}

async fn interactive_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<crate::interactive::StatusArgs>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
        ));
    }
    let token = bearer_token(&headers)?.ok_or_else(|| public_error(RegistryError::Unauthorized))?;
    crate::interactive::status(&state.registry, token, args)
        .await
        .map(Json)
        .map_err(public_interactive_error)
}

async fn interactive_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<crate::interactive::UpdateArgs>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
        ));
    }
    if state.scheduler.is_failed() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"scheduler_unavailable"})),
        ));
    }
    let token = bearer_token(&headers)?.ok_or_else(|| public_error(RegistryError::Unauthorized))?;
    crate::interactive::update(&state.registry, token, args)
        .await
        .map(Json)
        .map_err(public_interactive_error)
}

async fn interactive_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(args): Json<crate::interactive::CancelArgs>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
        ));
    }
    let token = bearer_token(&headers)?.ok_or_else(|| public_error(RegistryError::Unauthorized))?;
    crate::interactive::cancel(&state.registry, token, args)
        .await
        .map(Json)
        .map_err(public_interactive_error)
}

pub fn router(registry: Registry) -> Router {
    router_with_health(registry, SchedulerHealth::inspection_only())
}

fn router_with_health(registry: Registry, scheduler: SchedulerHealth) -> Router {
    Router::new()
        .route(
            "/health",
            get(|State(state): State<AppState>| async move {
                Json(json!({
                    "service":"agentisan",
                    "version":env!("CARGO_PKG_VERSION"),
                    "scheduler":state.scheduler.json()
                }))
            }),
        )
        .route("/v1/inspect", post(inspect))
        .route("/v1/team-action", post(team_action))
        .route("/v1/controller/run-start", post(controller_run_start))
        .route("/v1/interactive/start", post(interactive_start))
        .route("/v1/interactive/status", post(interactive_status))
        .route("/v1/interactive/update", post(interactive_update))
        .route("/v1/interactive/cancel", post(interactive_cancel))
        // A 16 KiB semantic result may require 6x that space when JSON-escaped.
        .layer(axum::extract::DefaultBodyLimit::max(131_072))
        .with_state(AppState {
            registry,
            scheduler,
        })
}

async fn scheduler(
    registry: Registry,
    data_dir: std::path::PathBuf,
    endpoint: String,
) -> anyhow::Result<()> {
    loop {
        match crate::teams::next(&registry).await {
            Ok(Some(work)) => {
                let result =
                    crate::worker::run(registry.clone(), work.clone(), &data_dir, &endpoint).await;
                crate::teams::finish(&registry, &work, result)
                    .await
                    .map_err(|error| anyhow::anyhow!("cannot record worker result: {error}"))?;
            }
            Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            Err(error) => return Err(anyhow::anyhow!("worker scheduler stopped: {error}")),
        }
    }
}

pub async fn serve(
    registry: Registry,
    listener: TcpListener,
    workers: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        anyhow::bail!("only loopback listeners are supported");
    }
    // Recovery is a startup fence, not a worker option. An inspection-only
    // service must never present a previous process's active turn as live.
    crate::teams::recover_interrupted(&registry).await?;
    let health = SchedulerHealth::inspection_only();
    let endpoint = format!("http://{}", listener.local_addr()?);
    let service = axum::serve(
        listener,
        router_with_health(registry.clone(), health.clone()),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .into_future();
    tokio::pin!(service);

    let Some(data_dir) = workers else {
        service.await?;
        return Ok(());
    };

    health.running();
    let mut task = tokio::spawn(scheduler(registry.clone(), data_dir, endpoint));
    tokio::select! {
        result = &mut service => {
            task.abort();
            let _ = task.await;
            crate::teams::recover_interrupted(&registry).await?;
            result?;
            Ok(())
        }
        outcome = &mut task => {
            health.failed();
            // Close the listener by dropping the server future. Persisted active
            // turns are fenced before returning the scheduler failure.
            crate::teams::recover_interrupted(&registry).await?;
            match outcome {
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(anyhow::anyhow!("worker scheduler stopped unexpectedly")),
                Err(error) => Err(anyhow::anyhow!("worker scheduler task failed: {error}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fixture,
        model::{AgentRole, Group, Team},
        teams::{MemberConfig, Provider, TeamConfig},
    };

    #[tokio::test]
    async fn running_scheduler_accepts_controller_start() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("state");
        let registry = Registry::open(&fixture::database_path(&data).unwrap())
            .await
            .unwrap();
        crate::teams::create(
            &registry,
            &data,
            &TeamConfig {
                group: Group {
                    id: "g".into(),
                    name: "Group".into(),
                },
                team: Team {
                    id: "t".into(),
                    group_id: "g".into(),
                    name: "Team".into(),
                },
                agents: vec![MemberConfig {
                    id: "lead".into(),
                    name: "Lead".into(),
                    role: AgentRole::Lead,
                    provider: Provider::Claude,
                    executable: std::env::current_exe().unwrap(),
                    model: "test".into(),
                    effort: "low".into(),
                    instructions: String::new(),
                }],
            },
        )
        .await
        .unwrap();
        let token = fixture::read_credential(&data.join("managed/t/lead.token")).unwrap();
        let scheduler = SchedulerHealth::inspection_only();
        scheduler.running();
        let state = AppState {
            registry: registry.clone(),
            scheduler,
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let result = controller_run_start(
            State(state),
            headers,
            Json(ControllerStartArgs {
                objective: "Execute the accepted plan".into(),
                live: true,
                idempotency_key: "desktop_start_1".into(),
                max_turns: None,
                max_messages: None,
                timeout_seconds: None,
                turn_timeout_seconds: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(result["team_id"], "t");
        assert_eq!(result["state"], "queued");
        assert_eq!(result["chat_identity"], "not_asserted");
        registry.close().await;
    }

    #[tokio::test]
    async fn running_scheduler_accepts_interactive_start_without_a_native_lead_turn() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("state");
        let registry = Registry::open(&fixture::database_path(&data).unwrap())
            .await
            .unwrap();
        crate::teams::create(
            &registry,
            &data,
            &TeamConfig {
                group: Group {
                    id: "interactive_group".into(),
                    name: "Interactive".into(),
                },
                team: Team {
                    id: "interactive_team".into(),
                    group_id: "interactive_group".into(),
                    name: "Interactive".into(),
                },
                agents: vec![
                    MemberConfig {
                        id: "lead".into(),
                        name: "Lead".into(),
                        role: AgentRole::Lead,
                        provider: Provider::Codex,
                        executable: std::env::current_exe().unwrap(),
                        model: "test".into(),
                        effort: "low".into(),
                        instructions: String::new(),
                    },
                    MemberConfig {
                        id: "worker".into(),
                        name: "Worker".into(),
                        role: AgentRole::Worker,
                        provider: Provider::Claude,
                        executable: std::env::current_exe().unwrap(),
                        model: "test".into(),
                        effort: "low".into(),
                        instructions: String::new(),
                    },
                ],
            },
        )
        .await
        .unwrap();
        let token =
            fixture::read_credential(&data.join("managed/interactive_team/lead.token")).unwrap();
        let scheduler = SchedulerHealth::inspection_only();
        scheduler.running();
        let state = AppState {
            registry: registry.clone(),
            scheduler,
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let result = interactive_start(
            State(state),
            headers,
            Json(crate::interactive::StartArgs {
                objective: "Use the worker directly".into(),
                live: true,
                connector_instance: "connector_test".into(),
                workers: vec!["worker".into()],
                initial_work: vec![crate::interactive::InitialWorkItem {
                    assignee: "worker".into(),
                    objective: "Review".into(),
                    done_criteria: vec!["Report evidence".into()],
                }],
                idempotency_key: "interactive_start_1".into(),
                max_turns: Some(2),
                max_messages: Some(4),
                timeout_seconds: Some(60),
                turn_timeout_seconds: Some(10),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(result["mode"], "interactive");
        let next = crate::teams::next(&registry).await.unwrap().unwrap();
        assert_eq!(next.agent.id.as_str(), "worker");
        registry.close().await;
    }
}
