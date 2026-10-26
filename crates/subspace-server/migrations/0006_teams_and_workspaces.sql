-- Rename the collaboration model: the top-level container "workspace" becomes
-- a "team", and the shared conversation "channel" becomes a "workspace".
-- Order matters: free the `workspace*` names before reusing them.

-- workspace -> team
ALTER TABLE workspaces RENAME TO teams;
ALTER TABLE workspace_members RENAME TO team_members;
ALTER TABLE team_members RENAME COLUMN workspace_id TO team_id;

-- channel -> workspace
ALTER TABLE channels RENAME TO workspaces;
ALTER TABLE workspaces RENAME COLUMN workspace_id TO team_id;

ALTER TABLE channel_members RENAME TO workspace_members;
ALTER TABLE workspace_members RENAME COLUMN channel_id TO workspace_id;

ALTER TABLE channel_posts RENAME TO workspace_posts;
ALTER TABLE workspace_posts RENAME COLUMN channel_id TO workspace_id;

DROP INDEX IF EXISTS idx_channel_posts;
CREATE INDEX IF NOT EXISTS idx_workspace_posts ON workspace_posts (workspace_id, seq);

-- Worker sessions and notices are keyed by conversation.
UPDATE messages SET conversation = 'workspace:' || substr(conversation, 9)
    WHERE conversation LIKE 'channel:%';
UPDATE notices SET conversation = 'workspace:' || substr(conversation, 9)
    WHERE conversation LIKE 'channel:%';
