-- How an operator post was routed when it mentioned nobody (JSON `Routing`).
ALTER TABLE channel_posts ADD COLUMN routing TEXT NOT NULL DEFAULT '{"state":"direct"}';
