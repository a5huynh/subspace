//! `/api/workspaces` and `/api/channels` REST handlers.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ApiResult, AppState};
use crate::WorkerId;
use crate::channel::{Channel, ChannelMember, ChannelPost, Workspace};
use crate::event::Notice;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/workspaces", get(list_workspaces))
        .route("/api/workspaces/{id}", get(workspace))
        .route("/api/workspaces/{id}/members", post(add_workspace_member))
        .route(
            "/api/workspaces/{id}/members/{worker}",
            delete(remove_workspace_member),
        )
        .route("/api/workspaces/{id}/channels", post(create_channel))
        .route(
            "/api/channels/{id}",
            get(channel).patch(update_channel).delete(delete_channel),
        )
        .route("/api/channels/{id}/members", post(add_member))
        .route("/api/channels/{id}/members/{worker}", delete(remove_member))
        .route("/api/channels/{id}/posts", post(create_post))
}

/// `GET /api/workspaces/{id}` response.
#[derive(Debug, Clone, Serialize, TS)]
pub struct WorkspaceDetail {
    pub workspace: Workspace,
    /// Member worker ids, in join order.
    pub members: Vec<WorkerId>,
    /// Channels, by name.
    pub channels: Vec<Channel>,
}

/// `GET /api/channels/{id}` response: everything to render a channel, plus
/// the stream position to resume from.
#[derive(Debug, Clone, Serialize, TS)]
pub struct ChannelDetail {
    /// The server's boot epoch (see `History.epoch`).
    pub epoch: String,
    /// Resume the channel stream with `since = cursor`. Taken before reading
    /// posts, so replayed events may repeat them; clients merge by id.
    pub cursor: u64,
    pub channel: Channel,
    pub members: Vec<ChannelMember>,
    pub posts: Vec<ChannelPost>,
    pub notices: Vec<Notice>,
}

/// Body for adding a worker to a workspace or channel.
#[derive(Debug, Clone, Deserialize, TS)]
pub struct AddMember {
    pub worker_id: WorkerId,
}

/// `POST /api/workspaces/{id}/channels` body.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(optional_fields)]
pub struct CreateChannel {
    pub name: String,
    pub topic: Option<String>,
    /// Initial worker members (must belong to the workspace).
    pub members: Option<Vec<WorkerId>>,
}

/// `PATCH /api/channels/{id}` body. Only provided fields change.
#[derive(Debug, Clone, Default, Deserialize, TS)]
#[ts(optional_fields)]
pub struct UpdateChannel {
    pub name: Option<String>,
    pub topic: Option<String>,
}

/// `POST /api/channels/{id}/posts` body: post as the operator.
#[derive(Debug, Clone, Deserialize, TS)]
pub struct CreatePost {
    pub text: String,
}

async fn list_workspaces(State(manager): State<AppState>) -> ApiResult<Json<Vec<Workspace>>> {
    Ok(Json(manager.channels()?.workspaces().await?))
}

async fn workspace(
    State(manager): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<WorkspaceDetail>> {
    let hub = manager.channels()?;
    Ok(Json(WorkspaceDetail {
        workspace: hub.workspace(&id).await?,
        members: hub.workspace_members(&id).await?,
        channels: hub.channels(&id).await?,
    }))
}

async fn add_workspace_member(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<AddMember>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(body) = body?;
    manager
        .channels()?
        .add_workspace_member(&id, &body.worker_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_workspace_member(
    State(manager): State<AppState>,
    Path((id, worker)): Path<(String, WorkerId)>,
) -> ApiResult<StatusCode> {
    manager
        .channels()?
        .remove_workspace_member(&id, &worker)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_channel(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CreateChannel>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Channel>)> {
    let Json(body) = body?;
    let channel = manager
        .channels()?
        .create_channel(
            &id,
            &body.name,
            body.topic.as_deref().unwrap_or(""),
            &body.members.unwrap_or_default(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(channel)))
}

async fn channel(
    State(manager): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ChannelDetail>> {
    let hub = manager.channels()?;
    let channel = hub.channel(&id).await?;
    // Cursor first: anything that lands in between is replayed (and
    // deduplicated by id) rather than lost.
    let cursor = hub.log(&id).cursor();
    Ok(Json(ChannelDetail {
        epoch: manager.epoch().to_string(),
        cursor,
        members: hub.members(&id).await?,
        posts: hub.posts(&id).await?,
        notices: hub.notices(&id).await?,
        channel,
    }))
}

async fn update_channel(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<UpdateChannel>, JsonRejection>,
) -> ApiResult<Json<Channel>> {
    let Json(body) = body?;
    let channel = manager
        .channels()?
        .update_channel(&id, body.name.as_deref(), body.topic.as_deref())
        .await?;
    Ok(Json(channel))
}

async fn delete_channel(
    State(manager): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    manager.channels()?.delete_channel(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_member(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<AddMember>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(body) = body?;
    manager.channels()?.add_member(&id, &body.worker_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_member(
    State(manager): State<AppState>,
    Path((id, worker)): Path<(String, WorkerId)>,
) -> ApiResult<StatusCode> {
    manager.channels()?.remove_member(&id, &worker).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_post(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CreatePost>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<ChannelPost>)> {
    let Json(body) = body?;
    let post = manager.channels()?.post_operator(&id, &body.text).await?;
    Ok((StatusCode::CREATED, Json(post)))
}
