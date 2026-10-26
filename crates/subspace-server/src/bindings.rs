//! TypeScript bindings for the console, generated from the wire types with
//! `ts-rs`. The committed output lives in `console/src/api/generated/`; the
//! `ts_bindings` test fails when it is stale. Regenerate with:
//!
//! ```text
//! UPDATE_TS_BINDINGS=1 cargo test -p subspace-server --test ts_bindings
//! ```

use std::path::Path;

use anyhow::Result;
use ts_rs::{Config, TS};

use crate::api::{
    AddMember, ChannelDetail, ChannelStreamMessage, CreateChannel, CreatePost, CreateWorker,
    ErrorBody, Health, PromptRequest, StreamMessage, UpdateChannel, UpdateWorker, WorkerDetail,
    WorkspaceDetail,
};
use crate::channel::Workspace;
use crate::event::ServerEvent;
use crate::manager::{History, WorkerSummary};
use crate::worker::WorkerConfig;

/// Where the console expects the generated bindings, relative to the repo root.
pub const CONSOLE_BINDINGS_DIR: &str = "console/src/api/generated";

/// Export every wire type (and everything they reference) into `dir`.
pub fn export_ts_bindings(dir: &Path) -> Result<()> {
    // JSON numbers, not `bigint`: `u64`s on the wire are seqs and token counts.
    let cfg = Config::new().with_out_dir(dir).with_large_int("number");
    ServerEvent::export_all(&cfg)?;
    WorkerSummary::export_all(&cfg)?;
    WorkerConfig::export_all(&cfg)?;
    History::export_all(&cfg)?;
    WorkerDetail::export_all(&cfg)?;
    CreateWorker::export_all(&cfg)?;
    UpdateWorker::export_all(&cfg)?;
    PromptRequest::export_all(&cfg)?;
    Health::export_all(&cfg)?;
    ErrorBody::export_all(&cfg)?;
    StreamMessage::export_all(&cfg)?;
    Workspace::export_all(&cfg)?;
    WorkspaceDetail::export_all(&cfg)?;
    ChannelDetail::export_all(&cfg)?;
    AddMember::export_all(&cfg)?;
    CreateChannel::export_all(&cfg)?;
    UpdateChannel::export_all(&cfg)?;
    CreatePost::export_all(&cfg)?;
    ChannelStreamMessage::export_all(&cfg)?;
    Ok(())
}
