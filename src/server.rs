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

pub fn router(registry: Registry) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async {
                Json(json!({"service":"agentisan","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .route("/v1/inspect", post(inspect))
        .layer(axum::extract::DefaultBodyLimit::max(16_384))
        .with_state(registry)
}

pub async fn serve(registry: Registry, listener: TcpListener) -> anyhow::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        anyhow::bail!("only loopback listeners are supported");
    }
    axum::serve(listener, router(registry))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
