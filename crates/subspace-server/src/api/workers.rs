//! `/api/workers` REST handlers.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use subspace::types::ThinkingLevel;
use ts_rs::TS;

use super::{ApiError, ApiResult, AppState};
use crate::WorkerId;
use crate::manager::{History, ManagerError, WorkerSummary};
use crate::skills::SkillSelection;
use crate::worker::{WorkerConfig, WorkerStatus};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/workers", get(list).post(create))
        .route(
            "/api/workers/{id}",
            get(detail).patch(update).delete(remove),
        )
        .route("/api/workers/{id}/prompt", post(prompt))
        .route("/api/workers/{id}/history", get(history))
}

/// `POST /api/workers` body. Omitted fields take engine defaults.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(optional_fields)]
pub struct CreateWorker {
    pub name: String,
    pub description: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<ThinkingLevel>,
    pub skills: Option<SkillSelection>,
}

/// `PATCH /api/workers/{id}` body. Only provided fields change.
#[derive(Debug, Clone, Default, Deserialize, TS)]
#[ts(optional_fields)]
pub struct UpdateWorker {
    pub name: Option<String>,
    pub description: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<ThinkingLevel>,
    pub skills: Option<SkillSelection>,
}

/// `POST /api/workers/{id}/prompt` body.
#[derive(Debug, Clone, Deserialize, TS)]
pub struct PromptRequest {
    pub text: String,
}

/// A worker's full config plus its live status.
#[derive(Debug, Clone, Serialize, TS)]
pub struct WorkerDetail {
    pub config: WorkerConfig,
    pub status: WorkerStatus,
}

async fn list(State(manager): State<AppState>) -> Json<Vec<WorkerSummary>> {
    Json(manager.list())
}

async fn create(
    State(manager): State<AppState>,
    body: Result<Json<CreateWorker>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<WorkerDetail>)> {
    let Json(body) = body?;
    let mut cfg = WorkerConfig::new(body.name, body.role.unwrap_or_default());
    if let Some(description) = body.description {
        cfg.description = description;
    }
    if let Some(model) = body.model {
        cfg.model = model;
    }
    if let Some(thinking) = body.thinking {
        cfg.thinking = thinking;
    }
    if let Some(skills) = body.skills {
        cfg.skills = skills;
    }
    validate(&mut cfg)?;

    let id = manager.spawn(cfg).await?;
    Ok((StatusCode::CREATED, Json(detail_of(&manager, &id)?)))
}

async fn detail(
    State(manager): State<AppState>,
    Path(id): Path<WorkerId>,
) -> ApiResult<Json<WorkerDetail>> {
    Ok(Json(detail_of(&manager, &id)?))
}

async fn update(
    State(manager): State<AppState>,
    Path(id): Path<WorkerId>,
    body: Result<Json<UpdateWorker>, JsonRejection>,
) -> ApiResult<Json<WorkerDetail>> {
    let Json(body) = body?;
    let mut cfg = manager
        .get(&id)
        .ok_or_else(|| anyhow::Error::from(ManagerError::NotFound(id.clone())))?;
    if let Some(name) = body.name {
        cfg.name = name;
    }
    if let Some(description) = body.description {
        cfg.description = description;
    }
    if let Some(role) = body.role {
        cfg.role = role;
    }
    if let Some(model) = body.model {
        cfg.model = model;
    }
    if let Some(thinking) = body.thinking {
        cfg.thinking = thinking;
    }
    if let Some(skills) = body.skills {
        cfg.skills = skills;
    }
    validate(&mut cfg)?;

    manager.update(cfg).await?;
    Ok(Json(detail_of(&manager, &id)?))
}

async fn remove(
    State(manager): State<AppState>,
    Path(id): Path<WorkerId>,
) -> ApiResult<StatusCode> {
    manager.remove(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn prompt(
    State(manager): State<AppState>,
    Path(id): Path<WorkerId>,
    body: Result<Json<PromptRequest>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(body) = body?;
    if body.text.trim().is_empty() {
        return Err(ApiError::bad_request("prompt text must not be empty"));
    }
    manager.prompt(&id, body.text).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn history(
    State(manager): State<AppState>,
    Path(id): Path<WorkerId>,
) -> ApiResult<Json<History>> {
    Ok(Json(manager.history(&id).await?))
}

fn detail_of(manager: &AppState, id: &str) -> ApiResult<WorkerDetail> {
    let not_found = || anyhow::Error::from(ManagerError::NotFound(id.to_string()));
    Ok(WorkerDetail {
        config: manager.get(id).ok_or_else(not_found)?,
        status: manager.status(id).ok_or_else(not_found)?,
    })
}

/// Trim and check user-editable fields.
fn validate(cfg: &mut WorkerConfig) -> ApiResult<()> {
    cfg.name = cfg.name.trim().to_string();
    cfg.description = cfg.description.trim().to_string();
    cfg.model = cfg.model.trim().to_string();
    if cfg.name.is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    if cfg.model.is_empty() {
        return Err(ApiError::bad_request("model must not be empty"));
    }
    if cfg.skills.disabled.iter().any(|s| s.trim().is_empty()) {
        return Err(ApiError::bad_request(
            "disabled skill names must not be empty",
        ));
    }
    Ok(())
}
