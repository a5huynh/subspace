-- Errors and warnings from worker runs. Kept apart from `messages` so they are
-- shown to humans but never replayed to the model.

CREATE TABLE IF NOT EXISTS notices (
    id         TEXT PRIMARY KEY,
    worker_id  TEXT NOT NULL,
    level      TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_notices_worker ON notices (worker_id, created_at);
