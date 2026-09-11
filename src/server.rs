use crate::{
    model::Query,
    registry::{Registry, RegistryError},
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
    let values: Vec<_> = headers.get_all("authorization").iter().collect();
    let token = match values.as_slice() {
        [] => None,
        [header] => {
            let raw = header
                .to_str()
                .map_err(|_| public_error(RegistryError::Unauthorized))?;
            Some(
                raw.strip_prefix("Bearer ")
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| public_error(RegistryError::Unauthorized))?,
            )
        }
        _ => return Err(public_error(RegistryError::Unauthorized)),
    };
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
    } else if detail.contains("no owned active turn")
        || detail.contains("agent binding is required")
        || detail.contains("only the lead")
    {
        (StatusCode::FORBIDDEN, "action_not_permitted")
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
    let values: Vec<_> = headers.get_all("authorization").iter().collect();
    let token = match values.as_slice() {
        [h] => h
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|v| !v.is_empty()),
        _ => None,
    }
    .ok_or_else(|| public_error(RegistryError::Unauthorized))?;
    crate::teams::act(&state.registry, token, action)
        .await
        .map(Json)
        .map_err(public_action_error)
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
