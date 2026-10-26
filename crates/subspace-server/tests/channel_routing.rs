//! Channels end to end at the manager level: mention routing, agent-to-agent
//! replies, the hop limit, failures, and separate per-conversation sessions.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use common::{ScriptedProvider, is_router, last_user_text, prompt_and_wait, settle};
use subspace::types::new_id;
use subspace_server::channel::router::DEFAULT_ROUTER_MODEL;
use subspace_server::channel::{Author, HOP_LIMIT, Routing};
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
        if is_router(r) {
            r#"{"handle": null, "reason": "just a note"}"#.into()
        } else if r.system_prompt.as_deref() == Some("ROLE:Alpha") {
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

    // Explicit mentions skip the router.
    assert!(f.provider.requests().iter().all(|r| !is_router(r)));

    // An unaddressed note goes to the router, which can decide nobody answers.
    f.post(&ch, "just a note").await;
    settle(&f.manager, &ch, 3).await;
    let posts = f.manager.channels().unwrap().posts(&ch).await.unwrap();
    assert_eq!(
        posts[2].routing,
        Routing::Nobody {
            reason: "just a note".into()
        }
    );
    assert_eq!(
        f.provider.requests().len(),
        2,
        "one worker turn + one routing call"
    );
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

#[tokio::test]
async fn unaddressed_posts_go_to_the_best_fit_worker() {
    let f = fixture(|r| {
        if is_router(r) {
            // Pick by description, as the real router would.
            let text = last_user_text(r);
            if text.contains("New message:\n[@you] Draft") {
                r#"Sure: {"handle": "@Writer", "reason": "drafting is their job"}"#.into()
            } else {
                r#"{"handle": "researcher", "reason": "a factual question"}"#.into()
            }
        } else {
            format!("{} answering", r.system_prompt.as_deref().unwrap_or(""))
        }
    })
    .await;
    let researcher = f.worker("Researcher").await;
    let writer = f.worker("Writer").await;
    let hub = f.manager.channels().unwrap();
    let mut cfg = f.manager.get(&writer).unwrap();
    cfg.description = "Drafts announcements".into();
    f.manager.update(cfg).await.unwrap();
    let ch = f.channel("launch", &[&researcher, &writer]).await;

    f.post(&ch, "What changed in v2?").await;
    settle(&f.manager, &ch, 2).await;
    f.post(&ch, "Draft the announcement").await;
    settle(&f.manager, &ch, 4).await;

    let posts = hub.posts(&ch).await.unwrap();
    assert_eq!(
        posts[0].routing,
        Routing::Chosen {
            worker_id: researcher.clone(),
            reason: "a factual question".into()
        }
    );
    assert_eq!(posts[1].text, "ROLE:Researcher answering");
    assert_eq!(
        posts[2].routing,
        Routing::Chosen {
            worker_id: writer.clone(),
            reason: "drafting is their job".into()
        }
    );
    assert_eq!(posts[3].text, "ROLE:Writer answering");
    assert_eq!(
        posts[1].routing,
        Routing::Direct,
        "worker replies aren't routed"
    );

    // The routing call uses the router model and sees members, roles, and context.
    let routing: Vec<_> = f
        .provider
        .requests()
        .into_iter()
        .filter(is_router)
        .collect();
    assert_eq!(routing.len(), 2);
    assert!(routing.iter().all(|r| r.model == DEFAULT_ROUTER_MODEL));
    let prompt = last_user_text(&routing[1]);
    assert!(
        prompt.contains("- @writer (Writer): Drafts announcements\n  Role: ROLE:Writer"),
        "{prompt}"
    );
    assert!(
        prompt.contains(
            "Recent posts:\n[@you] What changed in v2?\n[@researcher] ROLE:Researcher answering"
        ),
        "{prompt}"
    );
}

#[tokio::test]
async fn a_lone_worker_answers_without_a_routing_call() {
    let f = fixture(|_| "on it".into()).await;
    let alpha = f.worker("Alpha").await;
    let ch = f.channel("solo", &[&alpha]).await;

    f.post(&ch, "anything new?").await;
    settle(&f.manager, &ch, 2).await;
    let posts = f.manager.channels().unwrap().posts(&ch).await.unwrap();
    assert!(matches!(&posts[0].routing, Routing::Chosen { worker_id, .. } if worker_id == &alpha));
    assert_eq!(f.provider.requests().len(), 1);
    assert!(!is_router(&f.provider.requests()[0]));
}

#[tokio::test]
async fn an_unusable_routing_answer_wakes_nobody_and_says_so() {
    let f = fixture(|r| {
        if is_router(r) {
            r#"{"handle": "ghost", "reason": "?"}"#.into()
        } else {
            "should not run".into()
        }
    })
    .await;
    let alpha = f.worker("Alpha").await;
    let beta = f.worker("Beta").await;
    let ch = f.channel("launch", &[&alpha, &beta]).await;

    f.post(&ch, "hello?").await;
    settle(&f.manager, &ch, 1).await;
    let hub = f.manager.channels().unwrap();
    let posts = hub.posts(&ch).await.unwrap();
    assert_eq!(posts.len(), 1);
    assert!(matches!(&posts[0].routing, Routing::Failed { error } if error.contains("@ghost")));
    let notices = hub.notices(&ch).await.unwrap();
    assert!(
        notices[0].text.contains("@mention someone"),
        "{}",
        notices[0].text
    );
    assert!(f.provider.requests().iter().all(is_router));
}

#[test]
fn router_answers_are_parsed_leniently_but_checked() {
    use subspace_server::channel::ChannelMember;
    use subspace_server::channel::router::{Candidate, parse_answer};
    let candidates = vec![Candidate {
        member: ChannelMember {
            worker_id: "w1".into(),
            name: "Writer".into(),
            handle: "writer".into(),
            description: String::new(),
        },
        role: String::new(),
    }];
    let chosen = |reason: &str| Routing::Chosen {
        worker_id: "w1".into(),
        reason: reason.into(),
    };
    assert_eq!(
        parse_answer(
            "```json\n{\"handle\": \"@writer\", \"reason\": \"copy\"}\n```",
            &candidates
        )
        .unwrap(),
        chosen("copy")
    );
    assert_eq!(
        parse_answer(r#"{"handle": "WRITER"}"#, &candidates).unwrap(),
        chosen("")
    );
    assert!(parse_answer("the writer", &candidates).is_err());
    assert!(parse_answer(r#"{"handle": 3}"#, &candidates).is_err());
}
