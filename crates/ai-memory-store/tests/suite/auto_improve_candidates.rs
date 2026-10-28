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
            .claim_auto_improve_scheduler_session(ws, proj, inside, inside_end)
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
