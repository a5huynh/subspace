//! `/api/teams` and `/api/workspaces` REST handlers.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ApiResult, AppState};
use crate::WorkerId;
use crate::event::Notice;
use crate::workspace::{Team, Workspace, WorkspaceMember, WorkspacePost};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/teams", get(list_teams))
        .route("/api/teams/{id}", get(team))
        .route("/api/teams/{id}/members", post(add_team_member))
        .route(
            "/api/teams/{id}/members/{worker}",
            delete(remove_team_member),
        )
        .route("/api/teams/{id}/workspaces", post(create_workspace))
        .route(
            "/api/workspaces/{id}",
            get(workspace)
                .patch(update_workspace)
                .delete(delete_workspace),
        )
        .route("/api/workspaces/{id}/members", post(add_member))
        .route(
            "/api/workspaces/{id}/members/{worker}",
            delete(remove_member),
        )
        .route("/api/workspaces/{id}/posts", post(create_post))
}

/// `GET /api/teams/{id}` response.
#[derive(Debug, Clone, Serialize, TS)]
pub struct TeamDetail {
    pub team: Team,
    /// Member worker ids, in join order.
    pub members: Vec<WorkerId>,
    /// Workspaces, by name.
    pub workspaces: Vec<Workspace>,
}

/// `GET /api/workspaces/{id}` response: everything to render a workspace, plus
/// the stream position to resume from.
#[derive(Debug, Clone, Serialize, TS)]
pub struct WorkspaceDetail {
    /// The server's boot epoch (see `History.epoch`).
    pub epoch: String,
    /// Resume the workspace stream with `since = cursor`. Taken before reading
    /// posts, so replayed events may repeat them; clients merge by id.
    pub cursor: u64,
    pub workspace: Workspace,
    pub members: Vec<WorkspaceMember>,
    pub posts: Vec<WorkspacePost>,
    pub notices: Vec<Notice>,
}

/// Body for adding a worker to a team or workspace.
#[derive(Debug, Clone, Deserialize, TS)]
pub struct AddMember {
    pub worker_id: WorkerId,
}

/// `POST /api/teams/{id}/workspaces` body.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(optional_fields)]
pub struct CreateWorkspace {
    pub name: String,
    pub topic: Option<String>,
    /// Initial worker members (must belong to the team).
    pub members: Option<Vec<WorkerId>>,
}

/// `PATCH /api/workspaces/{id}` body. Only provided fields change.
#[derive(Debug, Clone, Default, Deserialize, TS)]
#[ts(optional_fields)]
pub struct UpdateWorkspace {
    pub name: Option<String>,
    pub topic: Option<String>,
}

/// `POST /api/workspaces/{id}/posts` body: post as the operator.
#[derive(Debug, Clone, Deserialize, TS)]
pub struct CreatePost {
    pub text: String,
}

async fn list_teams(State(manager): State<AppState>) -> ApiResult<Json<Vec<Team>>> {
    Ok(Json(manager.workspaces()?.teams().await?))
}

async fn team(
    State(manager): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<TeamDetail>> {
    let hub = manager.workspaces()?;
    Ok(Json(TeamDetail {
        team: hub.team(&id).await?,
        members: hub.team_members(&id).await?,
        workspaces: hub.workspaces(&id).await?,
    }))
}

async fn add_team_member(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<AddMember>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(body) = body?;
    manager
        .workspaces()?
        .add_team_member(&id, &body.worker_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_team_member(
    State(manager): State<AppState>,
    Path((id, worker)): Path<(String, WorkerId)>,
) -> ApiResult<StatusCode> {
    manager
        .workspaces()?
        .remove_team_member(&id, &worker)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_workspace(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CreateWorkspace>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Workspace>)> {
    let Json(body) = body?;
    let workspace = manager
        .workspaces()?
        .create_workspace(
            &id,
            &body.name,
            body.topic.as_deref().unwrap_or(""),
            &body.members.unwrap_or_default(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(workspace)))
}

async fn workspace(
    State(manager): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<WorkspaceDetail>> {
    let hub = manager.workspaces()?;
    let workspace = hub.workspace(&id).await?;
    // Cursor first: anything that lands in between is replayed (and
    // deduplicated by id) rather than lost.
    let cursor = hub.log(&id).cursor();
    Ok(Json(WorkspaceDetail {
        epoch: manager.epoch().to_string(),
        cursor,
        members: hub.members(&id).await?,
        posts: hub.posts(&id).await?,
        notices: hub.notices(&id).await?,
        workspace,
    }))
}

async fn update_workspace(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<UpdateWorkspace>, JsonRejection>,
) -> ApiResult<Json<Workspace>> {
    let Json(body) = body?;
    let workspace = manager
        .workspaces()?
        .update_workspace(&id, body.name.as_deref(), body.topic.as_deref())
        .await?;
    Ok(Json(workspace))
}

async fn delete_workspace(
    State(manager): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    manager.workspaces()?.delete_workspace(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_member(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<AddMember>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(body) = body?;
    manager
        .workspaces()?
        .add_member(&id, &body.worker_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_member(
    State(manager): State<AppState>,
    Path((id, worker)): Path<(String, WorkerId)>,
) -> ApiResult<StatusCode> {
    manager.workspaces()?.remove_member(&id, &worker).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_post(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CreatePost>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<WorkspacePost>)> {
    let Json(body) = body?;
    let post = manager.workspaces()?.post_operator(&id, &body.text).await?;
    Ok((StatusCode::CREATED, Json(post)))
}
