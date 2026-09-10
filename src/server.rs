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
use tokio::net::TcpListener;

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
    State(registry): State<Registry>,
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
    registry
        .inspect(token, query)
        .await
        .map(Json)
        .map_err(public_error)
}

async fn team_action(
    State(registry): State<Registry>,
    headers: HeaderMap,
    Json(action): Json<crate::teams::Action>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"browser_origin_not_allowed"})),
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
    crate::teams::act(&registry, token, action)
        .await
        .map(Json)
        .map_err(|e| {
            if e.downcast_ref::<RegistryError>()
                .is_some_and(|x| matches!(x, RegistryError::Unauthorized))
            {
                public_error(RegistryError::Unauthorized)
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":e.to_string()})),
                )
            }
        })
}

pub fn router(registry: Registry) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async {
                Json(json!({"service":"agentisan","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .route("/v1/inspect", post(inspect))
        .route("/v1/team-action", post(team_action))
        // A 16 KiB semantic result may require 6x that space when JSON-escaped.
        .layer(axum::extract::DefaultBodyLimit::max(131_072))
        .with_state(registry)
}

pub async fn serve(
    registry: Registry,
    listener: TcpListener,
    workers: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        anyhow::bail!("only loopback listeners are supported");
    }
    let task = if let Some(data_dir) = workers {
        crate::teams::recover_interrupted(&registry).await?;
        let registry = registry.clone();
        let endpoint = format!("http://{}", listener.local_addr()?);
        Some(tokio::spawn(async move {
            loop {
                match crate::teams::next(&registry).await {
                    Ok(Some(work)) => {
                        let result = crate::worker::run(
                            registry.clone(),
                            work.clone(),
                            &data_dir,
                            &endpoint,
                        )
                        .await;
                        if let Err(e) = crate::teams::finish(&registry, &work, result).await {
                            eprintln!("cannot record worker result: {e}");
                            break;
                        }
                    }
                    Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                    Err(e) => {
                        eprintln!("worker scheduler stopped: {e}");
                        break;
                    }
                }
            }
        }))
    } else {
        None
    };
    let result = axum::serve(listener, router(registry.clone()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
        crate::teams::recover_interrupted(&registry).await?;
    }
    result?;
    Ok(())
}
