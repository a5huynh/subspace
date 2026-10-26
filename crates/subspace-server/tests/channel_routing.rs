//! Channels end to end at the manager level: mention routing, agent-to-agent
//! replies, the hop limit, failures, and separate per-conversation sessions.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use common::{ScriptedProvider, last_user_text, prompt_and_wait, settle};
use subspace::types::new_id;
use subspace_server::channel::{Author, HOP_LIMIT};
use subspace_server::store::SqliteStore;
use subspace_server::{Homes, ManagerError, WorkerConfig, WorkerManager};

struct Fixture {
    manager: WorkerManager,
    provider: Arc<ScriptedProvider>,
    data: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.data);
    }
}

async fn fixture(
    script: impl Fn(&subspace::provider::CompletionRequest) -> String + Send + Sync + 'static,
) -> Fixture {
    let provider = Arc::new(ScriptedProvider::new(script));
    let data = std::env::temp_dir().join(format!("subspace_channels_test_{}", new_id()));
    fs::create_dir_all(&data).unwrap();
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
    let manager =
        WorkerManager::with_persistence(provider.clone(), store, Homes::open(&data).unwrap());
    Fixture {
        manager,
        provider,
        data,
    }
}

impl Fixture {
    async fn worker(&self, name: &str) -> String {
        // Roles double as the provider script's way to tell workers apart.
        self.manager
            .spawn(WorkerConfig::new(name, format!("ROLE:{name}")))
            .await
            .unwrap()
    }

    async fn channel(&self, name: &str, members: &[&String]) -> String {
        let hub = self.manager.channels().unwrap();
        let ws = &hub.workspaces().await.unwrap()[0];
        let members: Vec<String> = members.iter().map(|m| m.to_string()).collect();
        hub.create_channel(&ws.id, name, "", &members)
            .await
            .unwrap()
            .id
    }

    async fn post(&self, channel: &str, text: &str) {
        let hub = self.manager.channels().unwrap();
        hub.post_operator(channel, text).await.unwrap();
    }

    async fn transcript(&self, channel: &str) -> Vec<(String, String)> {
        let hub = self.manager.channels().unwrap();
        hub.posts(channel)
            .await
            .unwrap()
            .into_iter()
            .map(|p| (p.author_name, p.text))
            .collect()
    }
}

#[tokio::test]
async fn new_workers_join_the_default_workspace() {
    let f = fixture(|_| "ok".into()).await;
    let a = f.worker("Alpha").await;
    let hub = f.manager.channels().unwrap();
    let workspaces = hub.workspaces().await.unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].name, "General");
    assert_eq!(hub.workspace_members(&workspaces[0].id).await.unwrap(), [a]);
}

#[tokio::test]
async fn a_mention_wakes_only_that_worker_and_its_reply_is_posted() {
    let f = fixture(|r| {
        if r.system_prompt.as_deref() == Some("ROLE:Alpha") {
            "alpha here".into()
        } else {
            "beta here".into()
        }
    })
    .await;
    let alpha = f.worker("Alpha").await;
    let beta = f.worker("Beta").await;
    let ch = f.channel("launch", &[&alpha, &beta]).await;

    f.post(&ch, "@alpha status?").await;
    settle(&f.manager, &ch, 2).await;

    assert_eq!(
        f.transcript(&ch).await,
        [
            ("You".into(), "@alpha status?".into()),
            ("Alpha".into(), "alpha here".into())
        ]
    );
    assert!(f.provider.requests_from("ROLE:Beta").is_empty());

    // The turn shows the channel, the worker's identity, and the new post.
    let prompt = last_user_text(&f.provider.requests_from("ROLE:Alpha")[0]);
    assert!(
        prompt.contains("<channel name=\"launch\" workspace=\"General\">"),
        "{prompt}"
    );
    assert!(prompt.contains("You are @alpha."), "{prompt}");
    assert!(prompt.contains("[@you] @alpha status?"), "{prompt}");

    // A post without mentions wakes nobody.
    f.post(&ch, "just a note").await;
    settle(&f.manager, &ch, 3).await;
    assert_eq!(f.provider.requests().len(), 1);
}

#[tokio::test]
async fn workers_hand_off_to_each_other_and_catch_up_on_what_they_missed() {
    let f = fixture(|r| match r.system_prompt.as_deref() {
        Some("ROLE:Researcher") => "Found three changes. @writer please draft it.".into(),
        _ => "Draft: v2 ships three changes.".into(),
    })
    .await;
    let researcher = f.worker("Researcher").await;
    let writer = f.worker("Writer").await;
    let ch = f.channel("launch", &[&researcher, &writer]).await;

    f.post(&ch, "@researcher what changed in v2?").await;
    settle(&f.manager, &ch, 3).await;

    let posts = f.manager.channels().unwrap().posts(&ch).await.unwrap();
    assert_eq!(posts.len(), 3);
    assert_eq!(posts[1].author, Author::Worker(researcher.clone()));
    assert_eq!(posts[1].mentions, std::slice::from_ref(&writer));
    assert_eq!((posts[1].hops, posts[2].hops), (1, 2));
    assert!(posts.iter().all(|p| p.root_id == posts[0].id));

    // The writer wasn't mentioned by the operator, but sees that post too.
    let prompt = last_user_text(&f.provider.requests_from("ROLE:Writer")[0]);
    assert!(
        prompt.contains("[@you] @researcher what changed in v2?"),
        "{prompt}"
    );
    assert!(
        prompt.contains("[@researcher] Found three changes."),
        "{prompt}"
    );

    // Its next turn only shows what's new, and its own post isn't echoed back.
    f.post(&ch, "@writer shorter please").await;
    settle(&f.manager, &ch, 5).await;
    let prompt = last_user_text(f.provider.requests_from("ROLE:Writer").last().unwrap());
    assert!(prompt.contains("[@you] @writer shorter please"), "{prompt}");
    assert!(!prompt.contains("Found three changes"), "{prompt}");
    assert!(!prompt.contains("[@writer]"), "{prompt}");
}

#[tokio::test]
async fn the_hop_limit_stops_runaway_agent_conversations() {
    let f = fixture(|r| match r.system_prompt.as_deref() {
        Some("ROLE:Ping") => "@pong your turn".into(),
        _ => "@ping your turn".into(),
    })
    .await;
    let ping = f.worker("Ping").await;
    let pong = f.worker("Pong").await;
    let ch = f.channel("loop", &[&ping, &pong]).await;

    f.post(&ch, "@ping start").await;
    settle(&f.manager, &ch, 1 + HOP_LIMIT as usize).await;

    let hub = f.manager.channels().unwrap();
    let posts = hub.posts(&ch).await.unwrap();
    assert_eq!(
        posts.len(),
        1 + HOP_LIMIT as usize,
        "operator + {HOP_LIMIT} replies"
    );
    assert_eq!(f.provider.requests().len(), HOP_LIMIT as usize);
    let notices = hub.notices(&ch).await.unwrap();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].text.contains("Stopped after 5"),
        "{}",
        notices[0].text
    );

    // Posting again starts a new chain.
    f.post(&ch, "@pong go on").await;
    settle(&f.manager, &ch, 2 + 2 * HOP_LIMIT as usize).await;
    assert_eq!(hub.notices(&ch).await.unwrap().len(), 2);
}

#[tokio::test]
async fn channel_sessions_are_separate_from_the_dm() {
    let f = fixture(|r| {
        format!(
            "reply to: {}",
            last_user_text(r).lines().next().unwrap_or("")
        )
    })
    .await;
    let alpha = f.worker("Alpha").await;
    let ch = f.channel("launch", &[&alpha]).await;

    prompt_and_wait(&f.manager, &alpha, "dm one").await;
    f.post(&ch, "@alpha in channel").await;
    settle(&f.manager, &ch, 2).await;
    prompt_and_wait(&f.manager, &alpha, "dm two").await;

    let requests = f.provider.requests();
    let texts =
        |i: usize| -> Vec<String> { requests[i].messages.iter().map(|m| m.text()).collect() };
    // The channel turn starts a fresh session...
    assert_eq!(requests[1].messages.len(), 1);
    assert!(texts(1)[0].starts_with("<channel"));
    // ...and the DM continues without any channel content.
    assert_eq!(texts(2), ["dm one", "reply to: dm one", "dm two"]);
    assert!(
        f.manager
            .history(&alpha)
            .await
            .unwrap()
            .messages
            .iter()
            .all(|m| !m.text().contains("channel"))
    );
}

#[tokio::test]
async fn a_failed_turn_leaves_a_notice_instead_of_a_post() {
    let f = fixture(|_| String::new()).await;
    let alpha = f.worker("Alpha").await;
    let ch = f.channel("launch", &[&alpha]).await;

    f.post(&ch, "@alpha hello?").await;
    settle(&f.manager, &ch, 1).await;
    assert_eq!(
        f.transcript(&ch).await.len(),
        1,
        "empty replies aren't posted"
    );

    // A worker that's gone can't be woken; the channel says so.
    f.manager.shutdown(&alpha).await.unwrap();
    f.post(&ch, "@alpha anyone?").await;
    let notices = f.manager.channels().unwrap().notices(&ch).await.unwrap();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].text.contains("isn't running"),
        "{}",
        notices[0].text
    );
}

#[tokio::test]
async fn membership_rules() {
    let f = fixture(|_| "ok".into()).await;
    let hub = f.manager.channels().unwrap();
    let alpha = f.worker("Alpha").await;
    let twin = f.worker("alpha").await;
    let ch = f.channel("launch", &[&alpha]).await;
    let ws = hub.workspaces().await.unwrap()[0].id.clone();

    // Handles must be unique per channel.
    let err = hub.add_member(&ch, &twin).await.unwrap_err();
    assert!(
        matches!(err.downcast_ref(), Some(ManagerError::Conflict(_))),
        "{err}"
    );

    // Members must belong to the channel's workspace.
    let beta = f.worker("Beta").await;
    hub.remove_workspace_member(&ws, &beta).await.unwrap();
    let err = hub.add_member(&ch, &beta).await.unwrap_err();
    assert!(
        matches!(err.downcast_ref(), Some(ManagerError::Invalid(_))),
        "{err}"
    );

    // Leaving the workspace leaves its channels.
    hub.remove_workspace_member(&ws, &alpha).await.unwrap();
    assert!(hub.members(&ch).await.unwrap().is_empty());

    // Channel names are normalized and unique.
    let id = hub
        .create_channel(&ws, "Launch Plan!", "", &[])
        .await
        .unwrap();
    assert_eq!(id.name, "launch-plan");
    let err = hub
        .create_channel(&ws, "launch plan", "", &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err.downcast_ref(), Some(ManagerError::Conflict(_))),
        "{err}"
    );
    let err = hub.post_operator(&ch, "   ").await.unwrap_err();
    assert!(
        matches!(err.downcast_ref(), Some(ManagerError::Invalid(_))),
        "{err}"
    );
}

#[tokio::test]
async fn channels_need_persistence() {
    let manager = WorkerManager::new(Arc::new(ScriptedProvider::new(|_| String::new())));
    let err = manager.channels().err().unwrap();
    assert!(matches!(
        err.downcast_ref(),
        Some(ManagerError::Unsupported(_))
    ));
}
