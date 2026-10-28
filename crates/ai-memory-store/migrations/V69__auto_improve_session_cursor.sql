-- Per-session review cursor for segment review (FN8-10125, F2).
--
-- The scheduler used to review a session exactly once, after it ended. A long
-- or never-ended session (the common shape for AGY, OpenCode and Pool) was
-- never reviewed, or was reviewed from the top every time. The cursor records
-- how far a session has been reviewed, so the reviewer reads only the
-- observations after it and a segment is never reviewed twice.
--
--   last_observation_id          the last observation the reviewer received
--   last_observation_created_at  its created_at, kept beside the id because
--                                observation order is (created_at, id) and ids
--                                alone do not order backdated rows
--   last_reviewed_at             wall clock of the completed review
--   segments_reviewed            completed reviews of this session
--
-- A row exists only after a review COMPLETED; a failed review leaves the cursor
-- where it was, so the segment is retried under the claim attempt limits. The
-- row is removed with its session.

CREATE TABLE auto_improve_session_cursor (
    session_id                   BLOB PRIMARY KEY NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    last_observation_id          BLOB NOT NULL,
    last_observation_created_at  INTEGER NOT NULL,
    last_reviewed_at             INTEGER NOT NULL,
    segments_reviewed            INTEGER NOT NULL DEFAULT 1
);
