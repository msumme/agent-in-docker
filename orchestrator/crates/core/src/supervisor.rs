use std::time::{Duration, SystemTime};

/// Abstracts the current wall-clock time. Injectable so tests can control time.
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Classification of a `message_agent` handoff by sender/recipient role pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// A producer sent a message to the reviewer — review is being requested.
    ReviewRequested,
    /// The reviewer sent feedback back to a producer.
    Feedback,
    /// Any other role pair (planner→producer, etc.).
    Other,
}

fn is_producer(role: &str) -> bool {
    role == "feature-producer" || role == "maintenance-producer"
}

/// Classify a `message_agent` handoff by sender and recipient roles.
pub fn classify_handoff(from_role: &str, to_role: &str, _content: &str) -> Handoff {
    if is_producer(from_role) && to_role == "review-agent" {
        Handoff::ReviewRequested
    } else if from_role == "review-agent" && is_producer(to_role) {
        Handoff::Feedback
    } else {
        Handoff::Other
    }
}

/// Injectable: answers work-state questions about a team without causing side effects.
pub trait WorkProbe: Send + Sync {
    /// True if the team's producer clone has at least one commit (work was done).
    fn has_recent_commits(&self, team_id: &str) -> bool;
    /// True if the ticket has open blocking dependency tickets.
    fn open_blocking_deps(&self, ticket_id: &str) -> bool;
}

/// Diagnosis of a producer agent's activity state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Agent is active: last activity is within the threshold.
    Working,
    /// Idle past threshold, no commits, no blocking deps, no ping sent.
    Stalled,
    /// Idle past threshold but open blocking deps exist.
    Blocked,
    /// Idle past threshold with recent commits, no ping — may have finished silently.
    MaybeDone,
}

/// Diagnose the state of a producer agent given observable signals.
///
/// Pure function: no I/O, no `SystemTime::now()` calls, no side effects.
pub fn diagnose(
    now: SystemTime,
    last_activity: SystemTime,
    threshold: Duration,
    probe: &dyn WorkProbe,
    ping_observed: bool,
    team_id: &str,
    ticket_id: &str,
) -> Verdict {
    let elapsed = now.duration_since(last_activity).unwrap_or(Duration::ZERO);
    if elapsed < threshold {
        return Verdict::Working;
    }
    if probe.open_blocking_deps(ticket_id) {
        return Verdict::Blocked;
    }
    if probe.has_recent_commits(team_id) && !ping_observed {
        return Verdict::MaybeDone;
    }
    Verdict::Stalled
}

/// Format a SystemTime as a unix-epoch seconds string. Used for supervisor.log `ts` fields.
pub(crate) fn unix_secs_str(t: SystemTime) -> String {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Append a single JSON object as a newline-terminated record to a log file.
pub(crate) fn append_supervisor_log(path: &std::path::Path, entry: &serde_json::Value) {
    use std::io::Write;
    let Ok(mut line) = serde_json::to_string(entry) else { return };
    line.push('\n');
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProbe {
        commits: bool,
        deps: bool,
    }

    impl WorkProbe for FakeProbe {
        fn has_recent_commits(&self, _team_id: &str) -> bool {
            self.commits
        }
        fn open_blocking_deps(&self, _ticket_id: &str) -> bool {
            self.deps
        }
    }

    // --- classify_handoff ---

    #[test]
    fn classify_review_requested_feature_producer() {
        assert_eq!(
            classify_handoff("feature-producer", "review-agent", ""),
            Handoff::ReviewRequested
        );
    }

    #[test]
    fn classify_review_requested_maintenance_producer() {
        assert_eq!(
            classify_handoff("maintenance-producer", "review-agent", ""),
            Handoff::ReviewRequested
        );
    }

    #[test]
    fn classify_feedback_to_feature_producer() {
        assert_eq!(
            classify_handoff("review-agent", "feature-producer", ""),
            Handoff::Feedback
        );
    }

    #[test]
    fn classify_feedback_to_maintenance_producer() {
        assert_eq!(
            classify_handoff("review-agent", "maintenance-producer", ""),
            Handoff::Feedback
        );
    }

    #[test]
    fn classify_other_for_unrelated_roles() {
        assert_eq!(classify_handoff("planner", "feature-producer", ""), Handoff::Other);
        assert_eq!(classify_handoff("feature-producer", "planner", ""), Handoff::Other);
        assert_eq!(classify_handoff("review-agent", "planner", ""), Handoff::Other);
        assert_eq!(classify_handoff("planner", "review-agent", ""), Handoff::Other);
    }

    // --- diagnose ---

    const THRESHOLD: Duration = Duration::from_secs(300);

    fn make_now() -> SystemTime {
        // Use a fixed reference point so tests are deterministic.
        std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn diagnose_working_when_activity_is_recent() {
        let now = make_now();
        let last = now - Duration::from_secs(10); // 10s ago — within threshold
        let probe = FakeProbe { commits: false, deps: false };
        assert_eq!(
            diagnose(now, last, THRESHOLD, &probe, false, "team", "ticket"),
            Verdict::Working
        );
    }

    #[test]
    fn diagnose_stalled_when_idle_no_commits_no_deps_no_ping() {
        let now = make_now();
        let last = now - Duration::from_secs(600); // 10m — past threshold
        let probe = FakeProbe { commits: false, deps: false };
        assert_eq!(
            diagnose(now, last, THRESHOLD, &probe, false, "team", "ticket"),
            Verdict::Stalled
        );
    }

    #[test]
    fn diagnose_blocked_when_idle_and_has_open_deps() {
        let now = make_now();
        let last = now - Duration::from_secs(600);
        // Blocked regardless of whether there are commits
        let probe = FakeProbe { commits: true, deps: true };
        assert_eq!(
            diagnose(now, last, THRESHOLD, &probe, false, "team", "ticket"),
            Verdict::Blocked
        );
    }

    #[test]
    fn diagnose_blocked_ignores_commits_when_deps_open() {
        let now = make_now();
        let last = now - Duration::from_secs(600);
        let probe = FakeProbe { commits: false, deps: true };
        assert_eq!(
            diagnose(now, last, THRESHOLD, &probe, false, "team", "ticket"),
            Verdict::Blocked
        );
    }

    #[test]
    fn diagnose_maybe_done_when_idle_with_commits_and_no_ping() {
        let now = make_now();
        let last = now - Duration::from_secs(600);
        let probe = FakeProbe { commits: true, deps: false };
        assert_eq!(
            diagnose(now, last, THRESHOLD, &probe, false, "team", "ticket"),
            Verdict::MaybeDone
        );
    }

    #[test]
    fn diagnose_not_maybe_done_when_ping_already_observed() {
        let now = make_now();
        let last = now - Duration::from_secs(600);
        let probe = FakeProbe { commits: true, deps: false };
        // ping_observed=true: must NOT return MaybeDone
        let verdict = diagnose(now, last, THRESHOLD, &probe, true, "team", "ticket");
        assert_ne!(verdict, Verdict::MaybeDone);
    }

    #[test]
    fn diagnose_stalled_when_ping_observed_and_no_commits() {
        let now = make_now();
        let last = now - Duration::from_secs(600);
        let probe = FakeProbe { commits: false, deps: false };
        assert_eq!(
            diagnose(now, last, THRESHOLD, &probe, true, "team", "ticket"),
            Verdict::Stalled
        );
    }
}
