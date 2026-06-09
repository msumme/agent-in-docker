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
            "open" => Ok(TicketStatus::Open),
            other => Err(format!("bd show {}: unexpected status {:?}", ticket_id, other)),
        }
    }
}
