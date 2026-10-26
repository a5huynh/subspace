-- Phase B2: workspaces, channels, and per-conversation worker sessions.

-- Each worker has one session per conversation: its DM ('dm') plus one per
-- channel it takes part in ('channel:<id>'). Existing history is the DM.
ALTER TABLE messages ADD COLUMN conversation TEXT NOT NULL DEFAULT 'dm';
DROP INDEX IF EXISTS idx_messages_worker;
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages (worker_id, conversation, seq);

ALTER TABLE notices ADD COLUMN conversation TEXT NOT NULL DEFAULT 'dm';
CREATE INDEX IF NOT EXISTS idx_notices_conversation ON notices (conversation, created_at);

CREATE TABLE IF NOT EXISTS workspaces (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_members (
    workspace_id TEXT NOT NULL,
    worker_id    TEXT NOT NULL,
    joined_at    TEXT NOT NULL,
    PRIMARY KEY (workspace_id, worker_id)
);

CREATE TABLE IF NOT EXISTS channels (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name         TEXT NOT NULL,
    topic        TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    UNIQUE (workspace_id, name)
);

-- `last_seen_seq`: the last post the worker has been shown in this channel.
CREATE TABLE IF NOT EXISTS channel_members (
    channel_id    TEXT NOT NULL,
    worker_id     TEXT NOT NULL,
    last_seen_seq INTEGER NOT NULL DEFAULT 0,
    joined_at     TEXT NOT NULL,
    PRIMARY KEY (channel_id, worker_id)
);

-- The public transcript. `author_name` is captured at post time so posts
-- still read sensibly after a worker is renamed or deleted.
CREATE TABLE IF NOT EXISTS channel_posts (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    channel_id  TEXT NOT NULL,
    author_kind TEXT NOT NULL,
    author_id   TEXT,
    author_name TEXT NOT NULL,
    text        TEXT NOT NULL,
    mentions    TEXT NOT NULL,
    root_id     TEXT NOT NULL,
    hops        INTEGER NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_channel_posts ON channel_posts (channel_id, seq);
