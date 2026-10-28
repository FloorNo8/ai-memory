//! Scheduler candidate selection (FN8-10125).
//!
//! F1: the cheap preflight (observation floor, duration floor, boundary-only
//! sessions) is part of candidate selection, so a session that fails it never
//! gets a claim or a run row and so never spends a scheduler slot. The one-time
//! backfill lowers a scope's watermark to the horizon floor.

use ai_memory_core::{
    AgentKind, NewObservation, NewSession, ObservationKind, ProjectId, Sanitized, Sanitizer,
    SessionId, WorkspaceId,
};
use ai_memory_store::{AutoImproveCandidateCriteria, Store};
use rusqlite::{Connection, params};

const SEC: i64 = 1_000_000;
const DAY: i64 = 86_400 * SEC;

pub(crate) struct Fx {
    pub store: Store,
    pub ws: WorkspaceId,
    pub proj: ProjectId,
    _tmp: tempfile::TempDir,
}

pub(crate) async fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default".to_string())
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "p".to_string(), None)
        .await
        .unwrap();
    store
        .writer
        .ensure_auto_improve_scheduler_state(ws, proj)
        .await
        .unwrap();
    Fx {
        store,
        ws,
        proj,
        _tmp: tmp,
    }
}

pub(crate) fn now_us() -> i64 {
    jiff::Timestamp::now().as_microsecond()
}

/// Begin a session whose observations are spaced `step_secs` apart ending at
/// `last_at`. The session stays open.
pub(crate) async fn seed(
    fx: &Fx,
    kinds: &[ObservationKind],
    last_at: i64,
    step_secs: i64,
) -> SessionId {
    let id = SessionId::new();
    let first_at = last_at - step_secs * SEC * (kinds.len() as i64 - 1).max(0);
    fx.store
        .writer
        .begin_session(NewSession {
            occurred_at: Some(first_at),
            id,
            workspace_id: fx.ws,
            project_id: fx.proj,
            agent_kind: AgentKind::OpenCode,
            cwd: None,
            actor_user: None,
        })
        .await
        .unwrap();
    for (i, kind) in kinds.iter().enumerate() {
        add(fx, id, *kind, first_at + step_secs * SEC * i as i64).await;
    }
    id
}

pub(crate) async fn add(fx: &Fx, id: SessionId, kind: ObservationKind, at: i64) {
    fx.store
        .writer
        .insert_observation(Sanitized::new(
            NewObservation {
                occurred_at: Some(at),
                session_id: id,
                workspace_id: fx.ws,
                project_id: fx.proj,
                kind,
                extension: None,
                source_event: None,
                title: format!("{kind:?}"),
                body: "body".into(),
                importance: 5,
            },
            &Sanitizer::builtin(),
        ))
        .await
        .unwrap();
}

pub(crate) fn end_at(fx: &Fx, id: SessionId, at: i64) {
    let conn = Connection::open(fx.store.db_path()).unwrap();
    conn.execute(
        "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
        params![id.as_bytes(), at],
    )
    .unwrap();
}

pub(crate) fn count(fx: &Fx, table: &str) -> i64 {
    let conn = Connection::open(fx.store.db_path()).unwrap();
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

pub(crate) fn criteria() -> AutoImproveCandidateCriteria {
    AutoImproveCandidateCriteria {
        min_observations: 8,
        min_session_duration_secs: 120,
        require_substantive: true,
        ..AutoImproveCandidateCriteria::unfiltered(0)
    }
}

pub(crate) async fn candidates(fx: &Fx) -> Vec<SessionId> {
    fx.store
        .reader
        .auto_improve_candidate_sessions(fx.ws, fx.proj, criteria(), 50)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.session_id)
        .collect()
}

fn prompts(n: usize) -> Vec<ObservationKind> {
    vec![ObservationKind::UserPrompt; n]
}

#[tokio::test]
async fn a_session_below_the_observation_floor_is_not_a_candidate_and_leaves_no_trace() {
    let fx = fx().await;
    let id = seed(&fx, &prompts(3), now_us() - 3_600 * SEC, 60).await;
    end_at(&fx, id, now_us() - 1_800 * SEC);

    assert!(candidates(&fx).await.is_empty());
    assert_eq!(count(&fx, "auto_improve_scheduler_claims"), 0);
    assert_eq!(count(&fx, "auto_improve_runs"), 0);

    // New observations arrive: the same session becomes eligible again.
    for i in 0..6 {
        add(
            &fx,
            id,
            ObservationKind::UserPrompt,
            now_us() - 1_900 * SEC + i,
        )
        .await;
    }
    assert_eq!(candidates(&fx).await, vec![id]);
}

#[tokio::test]
async fn a_boundary_only_session_is_not_a_candidate() {
    let fx = fx().await;
    let kinds = [
        ObservationKind::SessionStart,
        ObservationKind::Stop,
        ObservationKind::Notification,
        ObservationKind::Stop,
        ObservationKind::Stop,
        ObservationKind::Notification,
        ObservationKind::Stop,
        ObservationKind::SessionEnd,
        ObservationKind::Stop,
        ObservationKind::Stop,
    ];
    let id = seed(&fx, &kinds, now_us() - 3_600 * SEC, 60).await;
    end_at(&fx, id, now_us() - 1_800 * SEC);
    assert!(candidates(&fx).await.is_empty());

    add(
        &fx,
        id,
        ObservationKind::PostToolUse,
        now_us() - 1_850 * SEC,
    )
    .await;
    assert_eq!(candidates(&fx).await, vec![id]);
}

#[tokio::test]
async fn a_session_shorter_than_the_duration_floor_is_not_a_candidate() {
    let fx = fx().await;
    // Eight prompts one second apart span 7 s, under the 120 s floor.
    let id = seed(&fx, &prompts(8), now_us() - 3_600 * SEC, 1).await;
    end_at(&fx, id, now_us() - 1_800 * SEC);
    assert!(candidates(&fx).await.is_empty());
    assert_eq!(count(&fx, "auto_improve_runs"), 0);
}

#[tokio::test]
async fn backfill_lowers_the_watermark_once_and_only_to_the_horizon() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default".to_string())
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "p".to_string(), None)
        .await
        .unwrap();
    let pre = Fx {
        store,
        ws,
        proj,
        _tmp: tmp,
    };

    // Two rich sessions that ended before the watermark is seeded: one inside
    // the 30-day horizon, one outside it. A third, newer one fixes the watermark.
    let inside = seed(&pre, &prompts(9), now_us() - 20 * DAY, 60).await;
    let inside_end = now_us() - 20 * DAY + 600 * SEC;
    end_at(&pre, inside, inside_end);
    let outside = seed(&pre, &prompts(9), now_us() - 45 * DAY, 60).await;
    end_at(&pre, outside, now_us() - 45 * DAY + 600 * SEC);
    let newest = seed(&pre, &prompts(9), now_us() - 2 * DAY, 60).await;
    end_at(&pre, newest, now_us() - 2 * DAY + 600 * SEC);

    // Project creation already seeded an empty-scope watermark; drop it so the
    // seed below sees the history, as a real upgrade does.
    Connection::open(pre.store.db_path())
        .unwrap()
        .execute("DELETE FROM auto_improve_scheduler_state", [])
        .unwrap();
    pre.store
        .writer
        .ensure_auto_improve_scheduler_state(ws, proj)
        .await
        .unwrap();
    assert!(
        candidates(&pre).await.is_empty(),
        "history under the startup watermark is not reviewed by default"
    );

    assert!(
        pre.store
            .writer
            .apply_auto_improve_backfill(ws, proj, 30 * 86_400)
            .await
            .unwrap(),
        "the first call applies the backfill"
    );
    let queue = candidates(&pre).await;
    assert!(queue.contains(&inside), "inside the horizon: eligible");
    assert!(
        queue.contains(&newest),
        "the newest session sat at the watermark"
    );
    assert!(
        !queue.contains(&outside),
        "outside the horizon: stays excluded"
    );

    // Reviewing `inside` (a claim) removes it; a second backfill must neither
    // re-queue it nor widen the window.
    assert!(
        pre.store
            .writer
            .claim_auto_improve_scheduler_session(ws, proj, inside, Some(inside_end))
            .await
            .unwrap()
    );
    assert!(
        !pre.store
            .writer
            .apply_auto_improve_backfill(ws, proj, 60 * 86_400)
            .await
            .unwrap(),
        "the backfill is one-shot per scope"
    );
    let queue = candidates(&pre).await;
    assert!(!queue.contains(&inside));
    assert!(
        !queue.contains(&outside),
        "a larger second horizon is ignored"
    );
}

// ---------------------------------------------------------------- F2: segments

fn open_criteria() -> AutoImproveCandidateCriteria {
    AutoImproveCandidateCriteria {
        idle_secs: Some(7_200),
        max_segment_observations: Some(200),
        compaction_trigger: true,
        ..criteria()
    }
}

async fn open_queue(fx: &Fx) -> Vec<ai_memory_store::AutoImproveCandidateSession> {
    fx.store
        .reader
        .auto_improve_candidate_sessions(fx.ws, fx.proj, open_criteria(), 50)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_open_session_idle_for_two_hours_becomes_eligible() {
    let fx = fx().await;
    // Last observation 2h01m ago: idle. 119 minutes ago: still working.
    let idle = seed(&fx, &prompts(9), now_us() - 7_260 * SEC, 30).await;
    let warm = seed(&fx, &prompts(9), now_us() - 7_140 * SEC, 30).await;

    assert!(
        candidates(&fx).await.is_empty(),
        "without the open-session triggers an open session is never a candidate"
    );
    let queue = open_queue(&fx).await;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].session_id, idle);
    assert_eq!(queue[0].trigger, ai_memory_store::SegmentTrigger::Idle);
    assert_eq!(queue[0].ended_at, None);
    assert!(queue.iter().all(|c| c.session_id != warm));
}

#[tokio::test]
async fn a_compaction_makes_a_recent_open_segment_eligible() {
    let fx = fx().await;
    let mut kinds = prompts(8);
    let plain = seed(&fx, &kinds, now_us() - 600 * SEC, 30).await;
    kinds.push(ObservationKind::PostCompaction);
    let compacted = seed(&fx, &kinds, now_us() - 600 * SEC, 30).await;
    let mut pre = prompts(8);
    pre.push(ObservationKind::PreCompact);
    let pre_compacted = seed(&fx, &pre, now_us() - 600 * SEC, 30).await;

    let queue = open_queue(&fx).await;
    let ids: Vec<_> = queue.iter().map(|c| c.session_id).collect();
    assert!(ids.contains(&compacted));
    assert!(ids.contains(&pre_compacted));
    assert!(
        !ids.contains(&plain),
        "no trigger fired for the plain session"
    );
    assert!(
        queue
            .iter()
            .all(|c| c.trigger == ai_memory_store::SegmentTrigger::Compaction)
    );
}

#[tokio::test]
async fn the_backstop_makes_a_busy_open_segment_eligible() {
    let fx = fx().await;
    let busy = seed(&fx, &prompts(200), now_us() - 60 * SEC, 1).await;
    let almost = seed(&fx, &prompts(199), now_us() - 60 * SEC, 1).await;
    // 200 observations one second apart still clear the 120 s duration floor.
    let queue = open_queue(&fx).await;
    assert_eq!(queue.len(), 1, "{queue:?}");
    assert_eq!(queue[0].session_id, busy);
    assert_eq!(queue[0].trigger, ai_memory_store::SegmentTrigger::Backstop);
    assert_eq!(queue[0].new_observations, 200);
    assert_ne!(queue[0].session_id, almost);
}

#[tokio::test]
async fn the_cursor_keeps_a_segment_from_being_reviewed_twice() {
    let fx = fx().await;
    let start = now_us() - 10_000 * SEC;
    let id = seed(&fx, &prompts(9), start, 30).await;

    let first = open_queue(&fx).await;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].after, None);
    let through = first[0].through.unwrap();
    assert!(
        fx.store
            .writer
            .claim_auto_improve_scheduler_session(fx.ws, fx.proj, id, None)
            .await
            .unwrap()
    );
    let segments = fx
        .store
        .writer
        .complete_auto_improve_segment(fx.ws, fx.proj, id, through)
        .await
        .unwrap();
    assert_eq!(segments, 1);
    assert!(
        open_queue(&fx).await.is_empty(),
        "the reviewed observations are not a segment any more"
    );
    let cursor = fx
        .store
        .reader
        .auto_improve_session_cursor(id)
        .await
        .unwrap();
    assert_eq!(cursor.unwrap().position, through);

    // Three new observations: under the floor of eight, no segment yet.
    for i in 0..3 {
        add(&fx, id, ObservationKind::UserPrompt, start + 100 * SEC + i).await;
    }
    assert!(open_queue(&fx).await.is_empty());
    // Six more reach the floor; the segment is exactly the nine new ones.
    for i in 0..6 {
        add(
            &fx,
            id,
            ObservationKind::PostToolUse,
            start + 300 * SEC + i * SEC,
        )
        .await;
    }
    let second = open_queue(&fx).await;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].new_observations, 9);
    assert_eq!(second[0].after, Some(through));

    // The reviewer is handed only observations after the cursor.
    let slice = fx
        .store
        .reader
        .observations_for_session_range(id, second[0].after, second[0].through)
        .await
        .unwrap();
    assert_eq!(slice.len(), 9);
    assert!(
        slice
            .iter()
            .all(|o| o.created_at.as_microsecond() > through.created_at)
    );
    let whole = fx
        .store
        .reader
        .observations_for_session_range(id, None, None)
        .await
        .unwrap();
    assert_eq!(whole.len(), 18);

    // The claim was released with the first segment, so the second can be taken.
    assert!(
        fx.store
            .writer
            .claim_auto_improve_scheduler_session(fx.ws, fx.proj, id, None)
            .await
            .unwrap()
    );
    let segments = fx
        .store
        .writer
        .complete_auto_improve_segment(fx.ws, fx.proj, id, second[0].through.unwrap())
        .await
        .unwrap();
    assert_eq!(segments, 2);
    assert!(open_queue(&fx).await.is_empty());

    // The cursor never moves backwards.
    fx.store
        .writer
        .complete_auto_improve_segment(fx.ws, fx.proj, id, through)
        .await
        .unwrap();
    let cursor = fx
        .store
        .reader
        .auto_improve_session_cursor(id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.position, second[0].through.unwrap());
}

#[tokio::test]
async fn a_failed_review_leaves_the_cursor_and_the_segment_is_retried() {
    let fx = fx().await;
    let id = seed(&fx, &prompts(9), now_us() - 10_000 * SEC, 30).await;
    assert!(
        fx.store
            .writer
            .claim_auto_improve_scheduler_session(fx.ws, fx.proj, id, None)
            .await
            .unwrap()
    );
    let attempts = fx
        .store
        .writer
        .record_auto_improve_claim_failure(fx.ws, fx.proj, id, "provider timeout")
        .await
        .unwrap();
    assert_eq!(attempts, 1);
    assert!(
        fx.store
            .reader
            .auto_improve_session_cursor(id)
            .await
            .unwrap()
            .is_none()
    );
    let queue = open_queue(&fx).await;
    assert_eq!(
        queue.len(),
        1,
        "retryable: the same segment is offered again"
    );
    assert_eq!(queue[0].after, None);
}

#[tokio::test]
async fn an_ended_session_is_re_reviewed_only_for_observations_after_its_cursor() {
    let fx = fx().await;
    let start = now_us() - 20_000 * SEC;
    let id = seed(&fx, &prompts(9), start, 30).await;
    end_at(&fx, id, start + 1_000 * SEC);

    let first = open_queue(&fx).await;
    assert_eq!(first[0].trigger, ai_memory_store::SegmentTrigger::Ended);
    assert_eq!(first[0].ended_at, Some(start + 1_000 * SEC));
    assert!(
        fx.store
            .writer
            .claim_auto_improve_scheduler_session(fx.ws, fx.proj, id, first[0].ended_at)
            .await
            .unwrap()
    );
    fx.store
        .writer
        .complete_auto_improve_segment(fx.ws, fx.proj, id, first[0].through.unwrap())
        .await
        .unwrap();
    assert!(open_queue(&fx).await.is_empty());

    // The session-end sweep adds boundary rows after the cursor: not a segment.
    for kind in [ObservationKind::Stop, ObservationKind::SessionEnd] {
        add(&fx, id, kind, start + 2_000 * SEC).await;
    }
    assert!(open_queue(&fx).await.is_empty());
}

#[tokio::test]
async fn an_open_session_under_the_watermark_is_history_unless_it_has_a_cursor() {
    let fx = fx().await;
    let id = seed(&fx, &prompts(9), now_us() - 10 * DAY, 30).await;
    // Lift the watermark above the session's last activity.
    Connection::open(fx.store.db_path())
        .unwrap()
        .execute(
            "UPDATE auto_improve_scheduler_state SET watermark_ended_at = ?1",
            params![now_us() - 5 * DAY],
        )
        .unwrap();
    assert!(
        open_queue(&fx).await.is_empty(),
        "stale open history stays out"
    );
    assert!(
        fx.store
            .reader
            .auto_improve_session_cursor(id)
            .await
            .unwrap()
            .is_none()
    );
}
