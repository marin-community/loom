-- Lookaside state + dynamic self-wake for overlookers.
--
-- `state` is a free-form JSON blob a watch program reads at the top of a round
-- and writes back at the end (the engine carries it across rounds). It is the
-- program's own scratch memory — e.g. a backoff watcher tracks, per session, how
-- many consecutive failures it has seen and when to retry next. Opaque to the
-- engine; the program owns its shape.
--
-- `wake_at` is a one-shot dynamic re-trigger: a round may ask the engine to fire
-- it again at a chosen time (`{wake_in: <secs>}` in its result), independent of
-- any cron cadence. The timer fires the tick once and clears the column, so a
-- watch can self-schedule its next look (an exponential-backoff recheck) instead
-- of polling on a fixed interval.
ALTER TABLE overlookers ADD COLUMN state   TEXT NOT NULL DEFAULT '{}';
ALTER TABLE overlookers ADD COLUMN wake_at TEXT;
