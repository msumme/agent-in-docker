use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::warn;

use crate::server::ServerState;
use crate::supervisor::{append_supervisor_log, diagnose, unix_secs_str, Verdict, WorkProbe};
use crate::types::OrchestratorEvent;

/// Default interval between watchdog passes.
pub const WATCHDOG_INTERVAL: Duration = Duration::from_secs(60);
/// Default idle threshold after which a producer is considered stalled/done.
pub const STALL_THRESHOLD: Duration = Duration::from_secs(300);

/// Real WorkProbe: shells to git and bd to answer observable work-state questions.
pub struct RealWorkProbe {
    pub clone_path: PathBuf,
    pub ticket_id: String,
}

impl WorkProbe for RealWorkProbe {
    fn has_recent_commits(&self, _team_id: &str) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&self.clone_path)
            .args(["log", "--oneline", "-1"])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    fn open_blocking_deps(&self, _ticket_id: &str) -> bool {
        // `bd deps <ticket> --open` lists open blocking deps; non-empty stdout = blocked.
        std::process::Command::new("bd")
            .args(["deps", &self.ticket_id, "--open"])
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
    }
}

/// Get the HEAD sha of a clone (short form). Falls back to "HEAD" on error.
fn head_sha(clone_path: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "HEAD".to_string())
}

/// One watchdog pass over all active teams. `make_probe` is injectable for testing.
pub(crate) async fn tick<F, P>(
    state: &Arc<Mutex<ServerState>>,
    project_root: &Path,
    threshold: Duration,
    make_probe: F,
)
where
    F: Fn(PathBuf, String) -> P,
    P: WorkProbe,
{
    let (producers, event_tx) = {
        let s = state.lock().await;
        (s.active_producer_agents(), s.event_tx_clone())
    };

    for (team_id, ticket_id, producer_name, clone_path, _reviewer_name) in producers {
        // Collect per-producer signals without holding the lock across I/O.
        let (now, last_act, already_fired, ping_observed) = {
            let s = state.lock().await;
            let now = s.clock_now();
            let last = s.last_activity_for(&producer_name);
            let fired = s.is_auto_fired(&team_id);
            let ping = s.is_handoff_observed(&team_id) || fired;
            (now, last, fired, ping)
        };

        if already_fired {
            continue;
        }

        let last_activity = match last_act {
            Some(t) => t,
            None => {
                // Producer has never touched the server — skip this pass.
                continue;
            }
        };

        let probe = make_probe(clone_path.clone(), ticket_id.clone());
        let verdict = diagnose(now, last_activity, threshold, &probe, ping_observed, &team_id, &ticket_id);

        // Emit event.
        let _ = event_tx.send(OrchestratorEvent::StallVerdict {
            team_id: team_id.clone(),
            agent: producer_name.clone(),
            verdict: format!("{:?}", verdict),
        });

        // Append to supervisor.log.
        let log_path = project_root.join(".teams").join(&team_id).join("supervisor.log");
        let ts = unix_secs_str(now);
        append_supervisor_log(
            &log_path,
            &serde_json::json!({
                "ts": ts,
                "team_id": team_id,
                "kind": "StallVerdict",
                "verdict": format!("{:?}", verdict),
            }),
        );

        if matches!(verdict, Verdict::MaybeDone) {
            let sha = head_sha(&clone_path);
            let mut s = state.lock().await;
            s.inject_review_request(&team_id, &sha);
        } else if matches!(verdict, Verdict::Blocked) {
            warn!(
                "stall_watchdog: team {} producer {} is Blocked",
                team_id, producer_name
            );
        }
    }
}

/// Spawn a background task that runs the stall watchdog on `interval`.
pub fn spawn(
    state: Arc<Mutex<ServerState>>,
    project_root: PathBuf,
    interval: Duration,
    threshold: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tick(
                &state,
                &project_root,
                threshold,
                |clone_path, ticket_id| RealWorkProbe { clone_path, ticket_id },
            )
            .await;
            tokio::time::sleep(interval).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{ServerState, UuidIdGenerator};
    use crate::supervisor::Clock;
    use crate::team_manager::{SpawnSpec, TeamManager, TeamState};
    use crate::types::OrchestratorEvent;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::{Duration, SystemTime};
    use tokio::sync::{mpsc, Mutex};

    // --- fakes ---

    struct FakeClock {
        now: StdMutex<SystemTime>,
    }

    impl FakeClock {
        fn at(t: SystemTime) -> Arc<Self> {
            Arc::new(Self { now: StdMutex::new(t) })
        }

        fn advance(&self, d: Duration) {
            let mut g = self.now.lock().unwrap();
            *g = *g + d;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> SystemTime {
            *self.now.lock().unwrap()
        }
    }

    struct FakeWorkProbe {
        commits: bool,
        deps: bool,
    }

    impl WorkProbe for FakeWorkProbe {
        fn has_recent_commits(&self, _: &str) -> bool { self.commits }
        fn open_blocking_deps(&self, _: &str) -> bool { self.deps }
    }

    struct FakeGit;
    impl crate::team_manager::GitOps for FakeGit {
        fn clone_local(&self, _src: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
            std::fs::create_dir_all(dest).unwrap();
            Ok(())
        }
        fn checkout_new_branch(&self, _: &std::path::Path, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn fetch_branch(&self, _: &std::path::Path, _: &std::path::Path, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn branch_delete(&self, _: &std::path::Path, _: &str) {}
    }

    fn make_state_with_team(
        event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
        clock: Arc<dyn Clock>,
    ) -> (Arc<Mutex<ServerState>>, String, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let mut tm = TeamManager::new(tmp.path().to_path_buf(), Box::new(FakeGit));
        let team = tm
            .create_team(SpawnSpec {
                ticket_id: "ticket-watchdog".into(),
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

        let team_id = team.id.clone();
        let mut state = ServerState::with_executor(
            event_tx,
            Arc::new(UuidIdGenerator),
            Arc::new(crate::server::RealRequestExecutor),
        );
        state.set_clock(clock);
        state.set_team_manager(Arc::new(StdMutex::new(tm)));

        // Register the producer and reviewer agents (so they appear in agents map).
        let (prod_tx, _) = mpsc::unbounded_channel();
        let (rev_tx, mut rev_rx_inner) = mpsc::unbounded_channel::<String>();
        let _ = state.register_agent(
            format!("{}-prod", team_id),
            "feature-producer".into(),
            Some(tmp.path().to_string_lossy().to_string()),
            prod_tx,
        );
        let _ = state.register_agent(
            format!("{}-rev", team_id),
            "review-agent".into(),
            None,
            rev_tx,
        );
        // Drain the peer_joined messages so tests start clean.
        while rev_rx_inner.try_recv().is_ok() {}

        (Arc::new(Mutex::new(state)), team_id, tmp)
    }

    #[tokio::test]
    async fn watchdog_autofire_exactly_once_on_maybe_done() {
        let base_time = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = FakeClock::at(base_time);
        let clock_arc: Arc<dyn Clock> = clock.clone();

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (state, team_id, tmp) = make_state_with_team(event_tx, clock_arc.clone());

        let producer_name = format!("{}-prod", team_id);
        std::fs::create_dir_all(tmp.path().join(".teams").join(&team_id)).unwrap();

        // Prime last_activity for the producer well before the threshold.
        {
            let mut s = state.lock().await;
            let past = base_time - Duration::from_secs(600);
            s.last_activity.insert(producer_name.clone(), past);
        }

        // Advance clock past the threshold.
        clock.advance(Duration::from_secs(1));

        // First tick: commits=true, deps=false → MaybeDone → auto-fire.
        tick(
            &state,
            tmp.path(),
            STALL_THRESHOLD,
            |_, _| FakeWorkProbe { commits: true, deps: false },
        )
        .await;

        // Drain events: there should be a StallVerdict(MaybeDone) and delivery messages
        // in the reviewer's channel. Check auto_fired is set.
        {
            let s = state.lock().await;
            assert!(s.is_auto_fired(&team_id), "auto_fired should be set after first tick");
        }

        // Collect all events from event_rx (non-blocking drain)
        let mut verdict_seen = false;
        while let Ok(ev) = event_rx.try_recv() {
            if let OrchestratorEvent::StallVerdict { verdict, .. } = ev {
                assert_eq!(verdict, "MaybeDone");
                verdict_seen = true;
            }
        }
        assert!(verdict_seen, "StallVerdict(MaybeDone) must be emitted on first tick");

        // The reviewer agent's sender should have received the auto-fire delivery.
        // We verify via auto_fired (delivery attempt was made) since the sender is internal.

        // Second tick: auto_fired is set → no new delivery, no new StallVerdict.
        tick(
            &state,
            tmp.path(),
            STALL_THRESHOLD,
            |_, _| FakeWorkProbe { commits: true, deps: false },
        )
        .await;

        // No new StallVerdict events should appear (producer was skipped due to auto_fired).
        assert!(
            event_rx.try_recv().is_err(),
            "second tick must not emit StallVerdict when already auto_fired"
        );
    }

    #[tokio::test]
    async fn watchdog_emits_blocked_verdict_and_no_autofire() {
        let base_time = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = FakeClock::at(base_time);
        let clock_arc: Arc<dyn Clock> = clock.clone();

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (state, team_id, tmp) = make_state_with_team(event_tx, clock_arc);

        let producer_name = format!("{}-prod", team_id);
        std::fs::create_dir_all(tmp.path().join(".teams").join(&team_id)).unwrap();

        {
            let mut s = state.lock().await;
            let past = base_time - Duration::from_secs(600);
            s.last_activity.insert(producer_name.clone(), past);
        }
        clock.advance(Duration::from_secs(1));

        // Drain setup events (AgentConnected x2) before asserting on tick output.
        while event_rx.try_recv().is_ok() {}

        // deps=true → Blocked.
        tick(
            &state,
            tmp.path(),
            STALL_THRESHOLD,
            |_, _| FakeWorkProbe { commits: true, deps: true },
        )
        .await;

        let ev = event_rx.try_recv().expect("StallVerdict must be emitted");
        assert!(
            matches!(&ev, OrchestratorEvent::StallVerdict { verdict, .. } if verdict == "Blocked"),
            "expected Blocked, got {:?}",
            ev
        );
        // No auto-fire for Blocked state.
        let s = state.lock().await;
        assert!(!s.is_auto_fired(&team_id), "Blocked state must not set auto_fired");

        // supervisor.log must contain a valid JSON line with at least ts and verdict.
        let log_path = tmp.path().join(".teams").join(&team_id).join("supervisor.log");
        let log_content = std::fs::read_to_string(&log_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(log_content.trim()).unwrap();
        assert!(parsed.get("ts").is_some());
        assert!(parsed.get("verdict").is_some());
        assert!(parsed.get("team_id").is_some());
    }
}
