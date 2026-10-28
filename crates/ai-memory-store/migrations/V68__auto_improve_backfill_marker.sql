-- One-time backfill marker for the auto-improve scheduler (FN8-10125, F1).
--
-- `watermark_ended_at` is seeded at startup to the newest already-ended session
-- so an upgrade never chews through history on its own. Recent history inside
-- the operator's horizon (30 days by default) is nevertheless worth reviewing
-- once. `backfill_applied_at` records that this scope's watermark has been
-- lowered to the horizon floor, so the lowering happens exactly once per scope
-- and a later restart or tick cannot re-open sessions that were already
-- reviewed, parked, or dropped.
--
-- NULL means "not yet applied". The column carries no default so every row
-- written by an older version reads as pending.

ALTER TABLE auto_improve_scheduler_state ADD COLUMN backfill_applied_at INTEGER;
