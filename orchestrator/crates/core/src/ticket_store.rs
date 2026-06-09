/// Outcome of a ticket status lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketStatus {
    Open,
    Closed,
    /// Ticket id does not exist in the store (was deleted or never valid).
    NotFound,
}

/// Abstraction over bd ticket state queries. Injectable for testing.
pub trait TicketStore: Send + Sync {
    fn status(&self, ticket_id: &str) -> Result<TicketStatus, String>;
}

pub struct RealTicketStore;

impl TicketStore for RealTicketStore {
    fn status(&self, ticket_id: &str) -> Result<TicketStatus, String> {
        let out = std::process::Command::new("bd")
            .args(["show", ticket_id, "--json"])
            .output()
            .map_err(|e| format!("bd show: {}", e))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("no issue found") {
                return Ok(TicketStatus::NotFound);
            }
            return Err(format!(
                "bd show {} failed: {}",
                ticket_id,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        let arr: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| format!("bd show {}: parse error: {}", ticket_id, e))?;

        let status = arr[0]["status"]
            .as_str()
            .ok_or_else(|| format!("bd show {}: missing status field", ticket_id))?;

        match status {
            "closed" => Ok(TicketStatus::Closed),
            "open" | "in_progress" | "blocked" => Ok(TicketStatus::Open),
            other => Err(format!("bd show {}: unexpected status {:?}", ticket_id, other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static PATH_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn make_fake_bd(json_status: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("bd");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '[{{\"status\":\"{}\"}}]'\n", json_status),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    fn with_fake_bd<T>(dir: &tempfile::TempDir, f: impl FnOnce() -> T) -> T {
        let _guard = PATH_MUTEX.lock().unwrap();
        let original = std::env::var("PATH").unwrap_or_default();
        // SAFETY: single-threaded via PATH_MUTEX
        unsafe { std::env::set_var("PATH", format!("{}:{}", dir.path().display(), original)) };
        let result = f();
        unsafe { std::env::set_var("PATH", original) };
        result
    }

    #[test]
    fn in_progress_maps_to_open() {
        let dir = make_fake_bd("in_progress");
        let store = RealTicketStore;
        let result = with_fake_bd(&dir, || store.status("some-ticket"));
        assert_eq!(result.unwrap(), TicketStatus::Open);
    }

    #[test]
    fn blocked_maps_to_open() {
        let dir = make_fake_bd("blocked");
        let store = RealTicketStore;
        let result = with_fake_bd(&dir, || store.status("some-ticket"));
        assert_eq!(result.unwrap(), TicketStatus::Open);
    }

    #[test]
    fn closed_maps_to_closed() {
        let dir = make_fake_bd("closed");
        let store = RealTicketStore;
        let result = with_fake_bd(&dir, || store.status("some-ticket"));
        assert_eq!(result.unwrap(), TicketStatus::Closed);
    }
}
