-- Phase A4: worker identity moves to on-disk homes; `workers` becomes an index
-- rebuilt from homes on boot. The A3 table is kept as `legacy_workers` so its
-- rows can be migrated into homes.

ALTER TABLE workers RENAME TO legacy_workers;

CREATE TABLE workers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    home_path   TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
