# Design: auto-improve review for every agent's sessions

**Status: approved for delivery 1 (F1 + F2), not yet implemented.** Tracking: FN8-10125. Decisions were settled with
the operator on 2026-10-28. The full research (harness hook surfaces, SOTA sources, Otto) is in the Linear document
attached to FN8-10125.

## Problem (measured 2026-10-28, read-only, `~/.fn8/ai-memory/db/memory.sqlite`, HEAD 22a0b321)

- **Who reaches the reviewer.** In 7 days, 24 runs reached the reviewer model (`poolside/laguna-s-2.1`). 20 of them
  came from automated Claude sessions in `~/.fn8/wt/fn8-os-runtime`. Only one run produced proposals.
- **Slot starvation.** The scheduler reviews one session per project per hour, oldest-ended first
  (`max_sessions_per_tick = 1`; `reader.rs:3472-3530`). The preflight check (`min_observations = 8`) runs only after
  the claim (`auto_improve.rs:1387-1415`). As a result, AGY sessions with 3 observations (start, stop, and a
  session-end synthesized by the finalize sweep) consume the slot. That was 358 skipped runs in 7 days; fn8-os alone
  has 1584 such sessions.
- **Open sessions are never reviewed.** The candidate predicate requires `ended_at IS NOT NULL` (`reader.rs:3499`).
  Open sessions with at least 8 observations in the last 30 days:

  | Agent | Open sessions |
  |---|---|
  | AGY | 203 |
  | OpenCode | 104 |
  | Pool | 65 |
  | Claude | 18 |
  | Cursor | 12 |
  | Codex | 1 |

- **No backfill.** The startup watermark (`auto_improve_schedule.rs:62`) never backfills, so 39 eligible sessions
  that ended between 09-28 and 10-08 are never reviewed.
- **Rejections are content-based, not infra.** When candidates do reach the model, they are rejected as
  `no_durable_lesson`, `one_off_task_narrative`, `tool_behavior_not_policy` or `session_specific_incident_detail`.
  The input selection is the bottleneck, not the reviewer.

## Decisions

| # | Decision | Rationale |
|---|---|---|
| Q1 | Delivery 1 = F1 + F2 (below). Per-agent adapters (AGY transcript, Muse, Cursor parity), default assistant-text capture and the recurrence gate are out of scope. | Fixes both gaps for agents already captured. |
| Q2 | Automated `fn8-os-runtime` sessions stay reviewable; no exclusion rule. | They produced the only validated proposals this week. Empty ones drop out through F1. |
| Q3 / Q6 | Burst budget for the backlog: 4 reviews per tick until the queue is empty, then automatically back to 1 per project per hour. | Poolside call volume is bounded; the funding model of the reviewer is unverified. |
| Q4 | A session segment becomes eligible after 2 h of inactivity (`idle_secs = 7200`). | Long sessions are reviewed several times without cutting short pauses into fragments. |
| Q5 | The backlog covers the last 30 days. | Recent work only; no review of stale history beyond that. |

## F1: preflight before claim, plus backfill

- **Filter in the candidate SQL.** Count observations since the session's review cursor (see F2) and require at
  least `min_observations`. Also exclude boundary-only sessions (`is_ephemeral_session`, `router.rs:3103`).
- **No slot consumption on rejection.** A session that fails the filter gets no claim row and no
  `auto_improve_runs` row. It stays eligible if new observations arrive.
- **Backfill.** Sessions in the 30-day horizon that sit under the watermark enter the queue once, under the burst
  budget.
- **Acceptance:**
  - zero scheduler slots consumed by preflight;
  - the 39 historical sessions are reviewed;
  - an eligible session reaches the model within one tick of becoming eligible.

## F2: review by session segment

- **New table** `auto_improve_session_cursor`, with columns `session_id`, `last_observation_id`, `last_reviewed_at`
  and `segments_reviewed`.
- **A segment becomes eligible at the first of:**
  1. session end;
  2. 2 h without a new observation;
  3. a compaction event: PreCompact or PostCompact for Claude Code, Codex, Cursor and Pool, `session.compacted` for
     OpenCode;
  4. the backstop `max_segment_observations` new observations (default 200).
- **Reviewer input.** The observations after the cursor, plus the compaction summary when the harness provides one
  (Claude Code `PostCompact.compact_summary`).
- **Cursor advance.** On a completed review the cursor moves to the last reviewed observation. On a failed review it
  stays, so the segment is retried under the existing attempt limits.
- **Scheduler budget.** Ticks stay non-overlapping (single writer). The burst budget (4 per tick) applies while the
  queue (backlog plus eligible segments) is non-empty; then it falls back to 1 per project per hour. The queue
  length is exported as a metric.
- **Acceptance:**
  - at least 50% of open rich sessions get one reviewed segment within 7 days;
  - repeated segments of one session never re-review the same observations.

## Metrics

Daily, per agent:
- runs that reached the model;
- proposals validated and approved;
- slots lost to preflight (target 0);
- open-session coverage;
- queue length.

## Out of scope (later phases)

- **F3:** AGY prompt and assistant text from `transcriptPath`, read incrementally by byte offset. Muse Code as a new
  `AgentKind` with its hook set.
- **F4:** Cursor CLI hook parity, Pool trajectory text, idle finalization for OpenCode, Pool and Cursor.
- **F5:**
  - a recurrence gate: a lesson must appear in at least 2 segments before promotion;
  - supersede links;
  - provenance quarantine and a trivial-prompt filter, both borrowed as design from Otto (openotto.io, Apache-2.0).
