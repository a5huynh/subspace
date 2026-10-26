//! HTTP API for the console: REST management under `/api`, plus the worker
//! event stream (WebSocket). Errors are always JSON: `{"error": "..."}`.

mod stream;
mod workers;

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use ts_rs::TS;

use crate::manager::{ManagerError, WorkerManager};

pub use stream::StreamMessage;
pub use workers::{CreateWorker, PromptRequest, UpdateWorker, WorkerDetail};

/// Shared state for all handlers.
pub type AppState = Arc<WorkerManager>;

/// The full API router. Mount at the server root; all routes live under `/api`.
pub fn router(manager: Arc<WorkerManager>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/workers/{id}/stream", get(stream::stream))
        .merge(workers::routes())
        .with_state(manager)
}

/// `GET /api/health` response.
#[derive(Debug, Clone, Serialize, TS)]
pub struct Health {
    pub status: String,
    /// The server's boot epoch (see `History.epoch`).
    pub epoch: String,
}

async fn health(axum::extract::State(manager): axum::extract::State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok".into(),
        epoch: manager.epoch().to_string(),
    })
}

/// The JSON body of every error response.
#[derive(Debug, Clone, Serialize, TS)]
pub struct ErrorBody {
    pub error: String,
}

/// An error that renders as `{"error": "..."}` with an HTTP status.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        let status = match err.downcast_ref::<ManagerError>() {
            Some(
                ManagerError::NotFound(_)
                | ManagerError::NoSuchWorkspace(_)
                | ManagerError::NoSuchChannel(_),
            ) => StatusCode::NOT_FOUND,
            Some(
                ManagerError::AlreadyExists(_) | ManagerError::Busy(_) | ManagerError::Conflict(_),
            ) => StatusCode::CONFLICT,
            Some(ManagerError::Invalid(_)) => StatusCode::BAD_REQUEST,
            Some(ManagerError::Unsupported(_)) => StatusCode::NOT_IMPLEMENTED,
            None => {
                tracing::error!("api error: {err:#}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        Self::new(status, format!("{err:#}"))
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
