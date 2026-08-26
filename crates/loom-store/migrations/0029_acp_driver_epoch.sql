-- Durable ownership fence for a session's live ACP driver. Bumped every time a
-- task subscribes to the relay, so a superseded driver (evicted by a newer
-- subscriber, in this process or in an overlapping restart generation) can no
-- longer stamp runtime status on a session it no longer drives.
ALTER TABLE sessions ADD COLUMN acp_driver_epoch INTEGER NOT NULL DEFAULT 0;
