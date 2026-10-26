//! Channel model: text helpers and storage.

use chrono::Utc;
use subspace::types::{Message, new_id};
use subspace_server::channel::{
    Author, Channel, ChannelMember, ChannelPost, Conversation, DEFAULT_WORKSPACE, Routing,
    handle_for, parse_mentions, render_turn,
};
use subspace_server::event::{Notice, NoticeLevel};
use subspace_server::store::SqliteStore;

fn post(channel: &str, author: Author, name: &str, text: &str) -> ChannelPost {
    let id = new_id();
    ChannelPost {
        id: id.clone(),
        channel_id: channel.to_string(),
        seq: 0,
        author,
        author_name: name.to_string(),
        text: text.to_string(),
        mentions: Vec::new(),
        root_id: id,
        hops: 0,
        routing: Routing::Direct,
        timestamp: Utc::now(),
    }
}

#[test]
fn handles_are_slugified_names() {
    assert_eq!(handle_for("Research Bot"), "research-bot");
    assert_eq!(handle_for("  E2E   Tester!! "), "e2e-tester");
    assert_eq!(handle_for("writer"), "writer");
    assert_eq!(handle_for("🤖"), "worker");
}

#[test]
fn mentions_are_distinct_lowercase_handles_not_emails() {
    assert_eq!(
        parse_mentions("@Writer can you and @research-bot, and @writer again? mail a@b.com"),
        vec!["writer", "research-bot"]
    );
    assert_eq!(parse_mentions("(@writer) @writer-. @_"), vec!["writer"]);
    assert!(parse_mentions("no mentions @ here").is_empty());
}

#[test]
fn a_turn_lists_members_identity_and_unseen_posts() {
    let channel = Channel {
        id: "c1".into(),
        workspace_id: "w".into(),
        name: "launch".into(),
        topic: "Ship v2".into(),
        created_at: Utc::now(),
    };
    let member = |id: &str, name: &str, description: &str| ChannelMember {
        worker_id: id.into(),
        name: name.into(),
        handle: handle_for(name),
        description: description.into(),
    };
    let members = vec![
        member("r", "Researcher", "finds facts"),
        member("w", "Writer", ""),
    ];
    let posts = vec![
        post("c1", Author::Operator, "You", "@researcher what changed?"),
        post(
            "c1",
            Author::Worker("r".into()),
            "Researcher",
            "Three things.",
        ),
    ];
    let text = render_turn("General", &channel, &members, &members[1], &posts, 2);
    assert!(text.contains("<channel name=\"launch\" workspace=\"General\">"));
    assert!(text.contains("Topic: Ship v2"));
    assert!(text.contains(
        "@you (human operator), @researcher (Researcher: finds facts), @writer (Writer)"
    ));
    assert!(text.contains("You are @writer."));
    assert!(text.contains("(2 earlier posts omitted)"));
    assert!(text.contains("[@you] @researcher what changed?\n[@researcher] Three things.\n"));
}

#[tokio::test]
async fn default_workspace_is_seeded_once() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let ws = store.ensure_default_workspace().await.unwrap();
    assert_eq!(ws.name, DEFAULT_WORKSPACE);
    assert_eq!(store.ensure_default_workspace().await.unwrap(), ws);
    assert_eq!(store.list_workspaces().await.unwrap(), vec![ws]);
}

#[tokio::test]
async fn channel_names_are_unique_per_workspace() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let a = store
        .create_channel("ws1", "launch", "")
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .create_channel("ws1", "launch", "")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .create_channel("ws2", "launch", "")
            .await
            .unwrap()
            .is_some()
    );
    let b = store
        .create_channel("ws1", "alpha", "")
        .await
        .unwrap()
        .unwrap();

    let names: Vec<_> = store
        .list_channels("ws1")
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["alpha", "launch"]);

    assert!(
        !store.update_channel(&b.id, "launch", "").await.unwrap(),
        "name taken"
    );
    assert!(
        store
            .update_channel(&a.id, "launch-v2", "Topic")
            .await
            .unwrap()
    );
    let a = store.get_channel(&a.id).await.unwrap().unwrap();
    assert_eq!((a.name.as_str(), a.topic.as_str()), ("launch-v2", "Topic"));
}

#[tokio::test]
async fn leaving_a_workspace_leaves_its_channels() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let ws = store.ensure_default_workspace().await.unwrap();
    let ch = store
        .create_channel(&ws.id, "launch", "")
        .await
        .unwrap()
        .unwrap();
    let other = store
        .create_channel("elsewhere", "x", "")
        .await
        .unwrap()
        .unwrap();
    store.add_workspace_member(&ws.id, "w1").await.unwrap();
    store.add_workspace_member(&ws.id, "w1").await.unwrap();
    store.add_channel_member(&ch.id, "w1").await.unwrap();
    store.add_channel_member(&other.id, "w1").await.unwrap();
    assert_eq!(store.workspace_members(&ws.id).await.unwrap(), ["w1"]);
    assert_eq!(
        store.worker_workspaces("w1").await.unwrap(),
        std::slice::from_ref(&ws.id)
    );

    store.remove_workspace_member(&ws.id, "w1").await.unwrap();
    assert!(store.workspace_members(&ws.id).await.unwrap().is_empty());
    assert!(store.channel_members(&ch.id).await.unwrap().is_empty());
    assert_eq!(
        store.channel_members(&other.id).await.unwrap(),
        ["w1"],
        "other workspace untouched"
    );
}

#[tokio::test]
async fn posts_roundtrip_and_last_seen_only_moves_forward() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let ch = store
        .create_channel("ws", "launch", "")
        .await
        .unwrap()
        .unwrap();
    store.add_channel_member(&ch.id, "w1").await.unwrap();

    let mut first = post(&ch.id, Author::Operator, "You", "@writer hi");
    first.mentions = vec!["w1".into()];
    first.seq = store.append_post(&first).await.unwrap();
    let mut reply = post(&ch.id, Author::Worker("w1".into()), "Writer", "hello");
    reply.root_id = first.id.clone();
    reply.hops = 1;
    reply.seq = store.append_post(&reply).await.unwrap();
    assert!(reply.seq > first.seq);

    // Timestamps go through RFC 3339 text; compare everything else exactly.
    let strip = |mut p: ChannelPost| {
        p.timestamp = chrono::DateTime::UNIX_EPOCH;
        p
    };
    let posts: Vec<_> = store
        .list_posts(&ch.id)
        .await
        .unwrap()
        .into_iter()
        .map(strip)
        .collect();
    assert_eq!(posts, vec![strip(first.clone()), strip(reply.clone())]);
    assert_eq!(store.posts_after(&ch.id, first.seq).await.unwrap().len(), 1);

    assert_eq!(store.last_seen(&ch.id, "w1").await.unwrap(), Some(0));
    assert_eq!(store.last_seen(&ch.id, "nobody").await.unwrap(), None);
    store.set_last_seen(&ch.id, "w1", reply.seq).await.unwrap();
    store.set_last_seen(&ch.id, "w1", first.seq).await.unwrap();
    assert_eq!(
        store.last_seen(&ch.id, "w1").await.unwrap(),
        Some(reply.seq)
    );
}

#[tokio::test]
async fn sessions_are_separate_per_conversation_and_deleting_a_channel_keeps_dms() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let ch = store
        .create_channel("ws", "launch", "")
        .await
        .unwrap()
        .unwrap();
    let in_channel = Conversation::Channel(ch.id.clone());
    store.add_channel_member(&ch.id, "w1").await.unwrap();
    store
        .append_messages("w1", &[Message::user("dm")])
        .await
        .unwrap();
    store
        .append_session("w1", &in_channel, &[Message::user("channel")])
        .await
        .unwrap();
    store
        .append_conversation_notice("w1", &in_channel, &Notice::new(NoticeLevel::Error, "boom"))
        .await
        .unwrap();
    store
        .append_post(&post(&ch.id, Author::Operator, "You", "hi"))
        .await
        .unwrap();

    assert_eq!(store.load_history("w1").await.unwrap()[0].text(), "dm");
    assert_eq!(
        store.load_session("w1", &in_channel).await.unwrap()[0].text(),
        "channel"
    );
    assert!(
        store.load_notices("w1").await.unwrap().is_empty(),
        "channel notices aren't DM notices"
    );
    assert_eq!(
        store
            .load_conversation_notices(&in_channel)
            .await
            .unwrap()
            .len(),
        1
    );

    store.delete_channel(&ch.id).await.unwrap();
    assert!(store.get_channel(&ch.id).await.unwrap().is_none());
    assert!(store.list_posts(&ch.id).await.unwrap().is_empty());
    assert!(store.channel_members(&ch.id).await.unwrap().is_empty());
    assert!(
        store
            .load_session("w1", &in_channel)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .load_conversation_notices(&in_channel)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.load_history("w1").await.unwrap().len(), 1, "DM kept");
}

#[tokio::test]
async fn deleting_a_worker_drops_its_memberships_but_keeps_its_posts() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let ws = store.ensure_default_workspace().await.unwrap();
    let ch = store
        .create_channel(&ws.id, "launch", "")
        .await
        .unwrap()
        .unwrap();
    store.add_workspace_member(&ws.id, "w1").await.unwrap();
    store.add_channel_member(&ch.id, "w1").await.unwrap();
    store
        .append_post(&post(&ch.id, Author::Worker("w1".into()), "Writer", "bye"))
        .await
        .unwrap();

    store.delete_worker("w1").await.unwrap();
    assert!(store.workspace_members(&ws.id).await.unwrap().is_empty());
    assert!(store.channel_members(&ch.id).await.unwrap().is_empty());
    assert_eq!(
        store.list_posts(&ch.id).await.unwrap()[0].author_name,
        "Writer"
    );
}

#[tokio::test]
async fn routing_is_stored_and_updated() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let mut p = post("c1", Author::Operator, "You", "who can help?");
    p.routing = Routing::Pending;
    store.append_post(&p).await.unwrap();
    assert_eq!(
        store.list_posts("c1").await.unwrap()[0].routing,
        Routing::Pending
    );

    let chosen = Routing::Chosen {
        worker_id: "w1".into(),
        reason: "writes copy".into(),
    };
    store.set_routing(&p.id, &chosen).await.unwrap();
    assert_eq!(store.list_posts("c1").await.unwrap()[0].routing, chosen);
}
