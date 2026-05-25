pub use crate::handlers::gh_pr::{PrLifecycle, PrState};

pub trait GhClient: Send + Sync {
    fn pr_state(&self, workspace: &str, number: u64) -> Result<PrState, String>;
}

pub struct RealGhClient;

impl GhClient for RealGhClient {
    fn pr_state(&self, workspace: &str, number: u64) -> Result<PrState, String> {
        crate::handlers::gh_pr::pr_state(workspace, number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::gh_pr::tests::{make_fake_gh, with_fake_gh};

    #[test]
    fn real_gh_client_parses_merged() {
        let dir = make_fake_gh(
            r#"printf '{"state":"MERGED","mergeCommit":{"oid":"abc123"}}'"#,
        );
        let client = RealGhClient;
        let result = with_fake_gh(&dir, || client.pr_state("", 1));
        let ps = result.unwrap();
        assert_eq!(ps.state, PrLifecycle::Merged);
        assert_eq!(ps.merge_commit.as_deref(), Some("abc123"));
    }

    #[test]
    fn real_gh_client_parses_open() {
        let dir = make_fake_gh(r#"printf '{"state":"OPEN"}'"#);
        let client = RealGhClient;
        let result = with_fake_gh(&dir, || client.pr_state("", 2));
        let ps = result.unwrap();
        assert_eq!(ps.state, PrLifecycle::Open);
        assert!(ps.merge_commit.is_none());
    }

    #[test]
    fn real_gh_client_parses_closed_no_merge_commit() {
        let dir = make_fake_gh(r#"printf '{"state":"CLOSED"}'"#);
        let client = RealGhClient;
        let result = with_fake_gh(&dir, || client.pr_state("", 3));
        let ps = result.unwrap();
        assert_eq!(ps.state, PrLifecycle::Closed);
        assert!(ps.merge_commit.is_none());
    }

    #[test]
    fn real_gh_client_returns_err_on_nonzero_exit() {
        let dir = make_fake_gh("echo 'not found' >&2; exit 1");
        let client = RealGhClient;
        let result = with_fake_gh(&dir, || client.pr_state("", 99));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed"));
    }
}
