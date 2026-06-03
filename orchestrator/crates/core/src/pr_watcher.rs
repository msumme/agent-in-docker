use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::warn;

use crate::gh_client::{GhClient, PrLifecycle};
use crate::server::ServerState;
use crate::types::OrchestratorEvent;

/// One reconciliation pass: poll every Active team with an open PR number and
/// emit `TeamPrMerged` / `TeamPrClosed` on the Open→{Merged,Closed} transition.
/// `last_seen` carries observed state across passes so a terminal state is
/// emitted exactly once. Absent from `last_seen` == assumed Open. Errors from
/// `gh` are logged and skipped — transient failures do not stop the watcher.
async fn poll_once(
    state: &Arc<Mutex<ServerState>>,
    gh: &Arc<dyn GhClient>,
    last_seen: &mut HashMap<(String, u64), PrLifecycle>,
) {
    let (teams, event_tx) = {
        let s = state.lock().await;
        (s.teams_with_open_pr(), s.event_tx_clone())
    };

    for (team_id, ticket_id, _work_branch, pr_number) in teams {
        match gh.pr_state("", pr_number) {
            Ok(pr_state) => {
                let key = (team_id.clone(), pr_number);
                let already_terminal = matches!(
                    last_seen.get(&key),
                    Some(PrLifecycle::Merged) | Some(PrLifecycle::Closed)
                );
                if already_terminal {
                    continue;
                }
                match pr_state.state {
                    PrLifecycle::Merged => {
                        last_seen.insert(key, PrLifecycle::Merged);
                        let _ = event_tx.send(OrchestratorEvent::TeamPrMerged {
                            team_id,
                            ticket_id,
                            pr_number,
                            merge_commit: pr_state.merge_commit,
                        });
                    }
                    PrLifecycle::Closed => {
                        last_seen.insert(key, PrLifecycle::Closed);
                        let _ = event_tx.send(OrchestratorEvent::TeamPrClosed {
                            team_id,
                            ticket_id,
                            pr_number,
                        });
                    }
                    PrLifecycle::Open => {
                        last_seen.insert(key, PrLifecycle::Open);
                    }
                }
            }
            Err(e) => {
                warn!("pr_watcher: PR #{} for team {}: {}", pr_number, team_id, e);
            }
        }
    }
}

/// Spawn a background task that reconciles PR state immediately on startup and
/// then every `interval`. The startup pass is what lets the orchestrator close
/// tickets whose PRs merged while it was down — previous runs persist on disk
/// as team manifests, so a fresh process sees them and acts on their PR fate
/// without waiting a full interval.
pub fn spawn(
    state: Arc<Mutex<ServerState>>,
    gh: Arc<dyn GhClient>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_seen: HashMap<(String, u64), PrLifecycle> = HashMap::new();
        loop {
            poll_once(&state, &gh, &mut last_seen).await;
            tokio::time::sleep(interval).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh_client::{PrLifecycle, PrState};
    use crate::server::{ServerState, UuidIdGenerator};
    use crate::team_manager::{SpawnSpec, TeamManager};
    use crate::types::OrchestratorEvent;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::{mpsc, Mutex};

    struct FakeGhClient {
        responses: StdMutex<Vec<Result<PrState, String>>>,
    }

    impl FakeGhClient {
        fn new(responses: Vec<Result<PrState, String>>) -> Self {
            Self {
                responses: StdMutex::new(responses),
            }
        }
    }

    impl GhClient for FakeGhClient {
        fn pr_state(&self, _workspace: &str, _number: u64) -> Result<PrState, String> {
            let mut v = self.responses.lock().unwrap();
            if v.is_empty() {
                return Ok(PrState {
                    state: PrLifecycle::Open,
                    merge_commit: None,
                });
            }
            v.remove(0)
        }
    }

    struct FakeGit;
    impl crate::team_manager::GitOps for FakeGit {
        fn clone_local(
            &self,
            _src: &std::path::Path,
            dest: &std::path::Path,
        ) -> Result<(), String> {
            std::fs::create_dir_all(dest).unwrap();
            Ok(())
        }
        fn checkout_new_branch(
            &self,
            _repo: &std::path::Path,
            _branch: &str,
            _base: &str,
        ) -> Result<(), String> {
            Ok(())
        }
        fn fetch_branch(
            &self,
            _canonical_repo: &std::path::Path,
            _src_clone: &std::path::Path,
            _branch: &str,
        ) -> Result<(), String> {
            Ok(())
        }
        fn branch_delete(&self, _repo: &std::path::Path, _branch: &str) {}
    }

    fn make_state_with_team(
        event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
        pr_number: u64,
    ) -> Arc<Mutex<ServerState>> {
        let tmp = tempfile::tempdir().unwrap();
        let mut tm = TeamManager::new(tmp.path().to_path_buf(), Box::new(FakeGit));
        let team = tm
            .create_team(SpawnSpec {
                ticket_id: "ticket-watcher".into(),
                base_branch: "main".into(),
                roles: vec![
                    ("planner".into(), "plan".into()),
                    ("feature-producer".into(), "prod".into()),
                    ("review-agent".into(), "rev".into()),
                ],
            })
            .unwrap()
            .clone();
        tm.mark_active(&team.id).unwrap();
        tm.set_pr(&team.id, "https://github.com/o/r/pull/1", pr_number)
            .unwrap();

        let mut state = ServerState::new(event_tx, Arc::new(UuidIdGenerator));
        state.set_team_manager(Arc::new(StdMutex::new(tm)));
        // Keep tmp alive by leaking it — temp dir stays on disk for test duration.
        std::mem::forget(tmp);
        Arc::new(Mutex::new(state))
    }

    #[tokio::test]
    async fn poll_emits_team_pr_merged_on_transition() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = make_state_with_team(event_tx, 1);
        let gh: Arc<dyn GhClient> = Arc::new(FakeGhClient::new(vec![
            Ok(PrState {
                state: PrLifecycle::Open,
                merge_commit: None,
            }),
            Ok(PrState {
                state: PrLifecycle::Merged,
                merge_commit: Some("abc".into()),
            }),
        ]));
        let mut last_seen = HashMap::new();

        // Pass 1 — Open, no event
        poll_once(&state, &gh, &mut last_seen).await;
        assert!(event_rx.try_recv().is_err(), "no event on first pass (Open)");

        // Pass 2 — Merged, TeamPrMerged event
        poll_once(&state, &gh, &mut last_seen).await;
        let event = event_rx.try_recv().expect("TeamPrMerged must be emitted");
        match event {
            OrchestratorEvent::TeamPrMerged {
                pr_number,
                merge_commit,
                ..
            } => {
                assert_eq!(pr_number, 1);
                assert_eq!(merge_commit.as_deref(), Some("abc"));
            }
            other => panic!("expected TeamPrMerged, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn poll_emits_team_pr_closed() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = make_state_with_team(event_tx, 2);
        let gh: Arc<dyn GhClient> = Arc::new(FakeGhClient::new(vec![Ok(PrState {
            state: PrLifecycle::Closed,
            merge_commit: None,
        })]));
        let mut last_seen = HashMap::new();

        poll_once(&state, &gh, &mut last_seen).await;

        let event = event_rx.try_recv().expect("TeamPrClosed must be emitted");
        assert!(
            matches!(event, OrchestratorEvent::TeamPrClosed { pr_number: 2, .. }),
            "got {:?}",
            event
        );
    }

    #[tokio::test]
    async fn poll_no_event_on_open() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = make_state_with_team(event_tx, 3);
        let gh: Arc<dyn GhClient> = Arc::new(FakeGhClient::new(vec![Ok(PrState {
            state: PrLifecycle::Open,
            merge_commit: None,
        })]));
        let mut last_seen = HashMap::new();

        poll_once(&state, &gh, &mut last_seen).await;

        assert!(
            event_rx.try_recv().is_err(),
            "no event expected when PR is still Open"
        );
    }

    #[tokio::test]
    async fn poll_emits_team_pr_closed_exactly_once_across_multiple_passes() {
        // Regression: if the team stays Active (TeamManager not updated between
        // passes), the watcher must not re-emit TeamPrClosed on later passes.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = make_state_with_team(event_tx, 5);
        let gh: Arc<dyn GhClient> = Arc::new(FakeGhClient::new(vec![
            Ok(PrState {
                state: PrLifecycle::Closed,
                merge_commit: None,
            }),
            Ok(PrState {
                state: PrLifecycle::Closed,
                merge_commit: None,
            }),
        ]));
        let mut last_seen = HashMap::new();

        // Pass 1 — Closed → exactly one TeamPrClosed emitted.
        poll_once(&state, &gh, &mut last_seen).await;
        let event = event_rx
            .try_recv()
            .expect("TeamPrClosed must be emitted on first Closed pass");
        assert!(
            matches!(event, OrchestratorEvent::TeamPrClosed { pr_number: 5, .. }),
            "got {:?}",
            event
        );

        // Pass 2 — still Closed, team still in manager → no second event.
        poll_once(&state, &gh, &mut last_seen).await;
        assert!(
            event_rx.try_recv().is_err(),
            "TeamPrClosed must not be re-emitted on subsequent passes"
        );
    }

    #[tokio::test]
    async fn poll_tolerates_gh_error_and_continues() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = make_state_with_team(event_tx, 4);
        let gh: Arc<dyn GhClient> = Arc::new(FakeGhClient::new(vec![
            Err("transient gh failure".into()),
            Ok(PrState {
                state: PrLifecycle::Merged,
                merge_commit: Some("def".into()),
            }),
        ]));
        let mut last_seen = HashMap::new();

        // Pass 1 — Err, no event, no state recorded
        poll_once(&state, &gh, &mut last_seen).await;
        assert!(event_rx.try_recv().is_err(), "no event on error pass");

        // Pass 2 — Merged, recovers
        poll_once(&state, &gh, &mut last_seen).await;
        let event = event_rx
            .try_recv()
            .expect("watcher must continue after error");
        assert!(matches!(event, OrchestratorEvent::TeamPrMerged { .. }));
    }

    #[tokio::test]
    async fn spawn_reconciles_immediately_on_startup() {
        // The startup pass must run before the first interval elapses — a freshly
        // launched orchestrator closes a ticket whose PR merged while it was down.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = make_state_with_team(event_tx, 7);
        let gh: Arc<dyn GhClient> = Arc::new(FakeGhClient::new(vec![Ok(PrState {
            state: PrLifecycle::Merged,
            merge_commit: Some("sha".into()),
        })]));

        tokio::time::pause();
        let _handle = spawn(state, gh, Duration::from_secs(300));
        // Yield without advancing time past the interval; the startup pass runs
        // before the watcher parks on its first sleep().
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        let event = event_rx
            .try_recv()
            .expect("startup pass must emit before first interval");
        assert!(matches!(
            event,
            OrchestratorEvent::TeamPrMerged { pr_number: 7, .. }
        ));
    }
}
