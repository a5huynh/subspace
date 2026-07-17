-- Phase A schema: workers and their message history.

CREATE TABLE IF NOT EXISTS workers (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    role       TEXT NOT NULL,
    model      TEXT NOT NULL,
    thinking   TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    worker_id  TEXT NOT NULL,
    data       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_worker ON messages (worker_id, seq);
