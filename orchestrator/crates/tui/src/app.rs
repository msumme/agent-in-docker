use orchestrator_core::types::*;
use serde_json::json;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Agents,
    Requests,
    PrReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    NewAgent,
    ConfirmQuit,
}

pub struct PendingRequest {
    pub agent_name: String,
    pub request_id: String,
    pub request_type: String,
    pub question: String,
}

/// A PR that was closed without merging — surfaced for human review.
#[derive(Debug, Clone)]
pub struct PendingPrReview {
    pub team_id: String,
    pub ticket_id: String,
    pub pr_number: u64,
}

pub struct App {
    pub agents: Vec<AgentInfo>,
    pub managed_agents: Vec<orchestrator_core::types::ManagedAgent>,
    pub pending_requests: Vec<PendingRequest>,
    pub pending_pr_review: Vec<PendingPrReview>,
    pub completed_log: Vec<String>,
    pub focus: FocusPanel,
    pub selected_agent: usize,
    pub selected_request: usize,
    pub selected_pr_review: usize,
    pub input_text: String,
    pub should_quit: bool,
    pub input_mode: InputMode,
    pub cmd_tx: mpsc::UnboundedSender<TuiCommand>,
}

impl App {
    pub fn new(cmd_tx: mpsc::UnboundedSender<TuiCommand>) -> Self {
        Self {
            agents: Vec::new(),
            managed_agents: Vec::new(),
            pending_requests: Vec::new(),
            pending_pr_review: Vec::new(),
            completed_log: Vec::new(),
            focus: FocusPanel::Requests,
            selected_agent: 0,
            selected_request: 0,
            selected_pr_review: 0,
            input_text: String::new(),
            should_quit: false,
            input_mode: InputMode::Normal,
            cmd_tx,
        }
    }

    pub fn handle_event(&mut self, event: OrchestratorEvent) {
        match event {
            OrchestratorEvent::AgentConnected(info) => {
                self.completed_log
                    .push(format!("+ {} ({}) connected", info.name, info.role));
                self.agents.push(info);
            }
            OrchestratorEvent::AgentDisconnected { id } => {
                if let Some(pos) = self.agents.iter().position(|a| a.id == id) {
                    let agent = self.agents.remove(pos);
                    self.completed_log
                        .push(format!("- {} disconnected", agent.name));
                    if self.selected_agent >= self.agents.len() && self.selected_agent > 0 {
                        self.selected_agent -= 1;
                    }
                }
            }
            OrchestratorEvent::RequestReceived {
                agent_name,
                request_id,
                request_type,
                payload,
                ..
            } => {
                let question = payload
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<no question>")
                    .to_string();
                self.pending_requests.push(PendingRequest {
                    agent_name,
                    request_id,
                    request_type,
                    question,
                });
                // Auto-focus requests panel when a new request arrives
                self.focus = FocusPanel::Requests;
                self.selected_request = self.pending_requests.len() - 1;
            }
            OrchestratorEvent::AgentOutput { agent_id: _, text } => {
                self.completed_log.push(text);
            }
            OrchestratorEvent::RequestExecuted {
                agent_id: _,
                agent_name,
                request_type,
                success: _,
                summary,
            } => {
                self.completed_log.push(format!(
                    "[{}] {} -> {}",
                    agent_name, request_type, summary
                ));
            }
            OrchestratorEvent::ManagedAgentUpdated(agent) => {
                self.completed_log.push(format!(
                    "[{}] status: {} - {}",
                    agent.name, agent.status, agent.last_activity
                ));
                // Update or insert in managed agents list
                if let Some(existing) = self.managed_agents.iter_mut().find(|a| a.name == agent.name) {
                    *existing = agent;
                } else {
                    self.managed_agents.push(agent);
                }
            }
            OrchestratorEvent::RequestAutoApproved { .. } => {}
            OrchestratorEvent::TeamPrMerged {
                team_id,
                ticket_id,
                pr_number,
                merge_commit,
            } => {
                let sha = merge_commit.as_deref().unwrap_or("unknown");
                let reason = format!("merged in PR #{} ({})", pr_number, sha);
                self.completed_log.push(format!(
                    "[team:{}] ticket {} closed: {}",
                    team_id, ticket_id, reason
                ));
                let _ = self.cmd_tx.send(TuiCommand::CloseAndTeardownTeam {
                    team_id,
                    ticket_id,
                    pr_number,
                    merge_commit,
                    reason,
                });
            }
            OrchestratorEvent::TeamPrClosed {
                team_id,
                ticket_id,
                pr_number,
            } => {
                self.completed_log.push(format!(
                    "[team:{}] PR #{} closed without merging — pending review",
                    team_id, pr_number
                ));
                self.pending_pr_review.push(PendingPrReview {
                    team_id,
                    ticket_id,
                    pr_number,
                });
            }
            OrchestratorEvent::HandoffObserved {
                team_id,
                kind,
                from,
                to,
            } => {
                self.completed_log.push(format!(
                    "[team:{}] handoff {} → {} ({})",
                    team_id, from, to, kind
                ));
            }
            OrchestratorEvent::StallVerdict {
                team_id,
                agent,
                verdict,
            } => {
                self.completed_log.push(format!(
                    "[team:{}] stall-watchdog: {} → {}",
                    team_id, agent, verdict
                ));
            }
        }
    }

    /// Forget the closed-PR entry at `idx` — close ticket as wontfix and tear down team.
    pub fn forget_pr_review(&mut self, idx: usize) {
        if idx >= self.pending_pr_review.len() {
            return;
        }
        let entry = self.pending_pr_review.remove(idx);
        self.completed_log.push(format!(
            "[team:{}] PR #{} forgotten (wontfix)",
            entry.team_id, entry.pr_number
        ));
        let _ = self.cmd_tx.send(TuiCommand::ForgetTeamPr {
            team_id: entry.team_id,
            ticket_id: entry.ticket_id,
            pr_number: entry.pr_number,
        });
        if self.selected_pr_review >= self.pending_pr_review.len() && self.selected_pr_review > 0 {
            self.selected_pr_review -= 1;
        }
    }

    pub fn submit_answer(&mut self) {
        // If no pending requests, send the input as a task to the selected agent
        if self.pending_requests.is_empty() {
            if self.input_text.is_empty() || self.agents.is_empty() {
                return;
            }
            let agent = &self.agents[self.selected_agent.min(self.agents.len() - 1)];
            self.completed_log.push(format!(
                "[{}] << {}",
                agent.name, self.input_text
            ));
            let _ = self.cmd_tx.send(TuiCommand::SendTask {
                agent_id: agent.id.clone(),
                prompt: self.input_text.clone(),
            });
            self.input_text.clear();
            return;
        }

        let idx = self.selected_request.min(self.pending_requests.len() - 1);
        let req = &self.pending_requests[idx];

        match req.request_type.as_str() {
            "user_prompt" => {
                if self.input_text.is_empty() {
                    return;
                }
                let req = self.pending_requests.remove(idx);
                self.completed_log.push(format!(
                    "[{}] Q: {} -> A: {}",
                    req.agent_name, req.question, self.input_text
                ));
                let payload = json!({ "answer": self.input_text });
                let _ = self.cmd_tx.send(TuiCommand::RespondToRequest {
                    request_id: req.request_id,
                    payload,
                });
                self.input_text.clear();
            }
            _ => {
                // For file_read, git_push etc: Enter approves
                self.approve_request();
                return;
            }
        }

        if self.selected_request >= self.pending_requests.len() && self.selected_request > 0 {
            self.selected_request -= 1;
        }
    }

    pub fn approve_request(&mut self) {
        if self.pending_requests.is_empty() {
            return;
        }
        let idx = self.selected_request.min(self.pending_requests.len() - 1);
        let req = self.pending_requests.remove(idx);

        self.completed_log.push(format!(
            "[{}] {} {} approval sent…",
            req.agent_name, req.request_type, req.question
        ));

        // Server handles execution and MCP resolution via ApproveRequest.
        // TUI only sends intent -- never executes I/O.
        let _ = self.cmd_tx.send(TuiCommand::ApproveRequest {
            request_id: req.request_id,
        });

        if self.selected_request >= self.pending_requests.len() && self.selected_request > 0 {
            self.selected_request -= 1;
        }
    }

    pub fn deny_request(&mut self) {
        if self.pending_requests.is_empty() {
            return;
        }
        let idx = self.selected_request.min(self.pending_requests.len() - 1);
        let req = self.pending_requests.remove(idx);

        self.completed_log.push(format!(
            "[{}] {} {} -> DENIED",
            req.agent_name, req.request_type, req.question
        ));
        let _ = self.cmd_tx.send(TuiCommand::DenyRequest {
            request_id: req.request_id,
            reason: "Denied by user".into(),
        });

        if self.selected_request >= self.pending_requests.len() && self.selected_request > 0 {
            self.selected_request -= 1;
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPanel::Agents => FocusPanel::Requests,
            FocusPanel::Requests => FocusPanel::PrReview,
            FocusPanel::PrReview => FocusPanel::Agents,
        };
    }

    pub fn move_selection_up(&mut self) {
        match self.focus {
            FocusPanel::Agents => {
                if self.selected_agent > 0 {
                    self.selected_agent -= 1;
                }
            }
            FocusPanel::Requests => {
                if self.selected_request > 0 {
                    self.selected_request -= 1;
                }
            }
            FocusPanel::PrReview => {
                if self.selected_pr_review > 0 {
                    self.selected_pr_review -= 1;
                }
            }
        }
    }

    /// Get the name of the currently selected agent (for attach mode).
    pub fn selected_agent_name(&self) -> Option<String> {
        if self.agents.is_empty() {
            return None;
        }
        let idx = self.selected_agent.min(self.agents.len() - 1);
        Some(self.agents[idx].name.clone())
    }

}

/// Effects returned by handle_key for the main loop to execute.
#[derive(Debug, PartialEq)]
pub enum KeyEffect {
        None,
        Quit,
        AttachAgent(String),
    }

impl App {
    /// Handle a key press. Returns an effect for the main loop to act on.
    /// All state transitions happen here -- main.rs only executes effects.
    pub fn handle_key(&mut self, code: crossterm::event::KeyCode) -> KeyEffect {
        use crossterm::event::KeyCode;

        if self.input_mode == InputMode::ConfirmQuit {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return KeyEffect::Quit,
                _ => { self.input_mode = InputMode::Normal; return KeyEffect::None; }
            }
        }

        if self.input_mode == InputMode::NewAgent {
            match code {
                KeyCode::Enter => {
                    if !self.input_text.is_empty() {
                        let parts: Vec<&str> = self.input_text.splitn(2, ':').collect();
                        let name = parts[0].trim().to_string();
                        let role = parts.get(1).map(|r| r.trim().to_string()).unwrap_or_else(|| "code-agent".into());
                        self.completed_log.push(format!("Starting agent '{}'...", name));
                        let _ = self.cmd_tx.send(TuiCommand::StartNewAgent { name, role });
                    }
                    self.input_text.clear();
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Esc => { self.input_text.clear(); self.input_mode = InputMode::Normal; }
                KeyCode::Backspace => { self.input_text.pop(); }
                KeyCode::Char(c) => { self.input_text.push(c); }
                _ => {}
            }
            return KeyEffect::None;
        }

        // Normal mode
        let approval_mode = self.focus == FocusPanel::Requests
            && !self.pending_requests.is_empty()
            && self.pending_requests
                [self.selected_request.min(self.pending_requests.len() - 1)]
                .request_type != "user_prompt";

        let pr_review_mode = self.focus == FocusPanel::PrReview
            && !self.pending_pr_review.is_empty();

        match code {
            KeyCode::Char('q') => { self.input_mode = InputMode::ConfirmQuit; }
            KeyCode::Char('N') => { self.input_mode = InputMode::NewAgent; self.input_text.clear(); }
            KeyCode::Char('r') if self.focus == FocusPanel::Agents => {
                if let Some(name) = self.selected_agent_name() {
                    let _ = self.cmd_tx.send(TuiCommand::ReattachAgent { name: name.clone() });
                    self.completed_log.push(format!("Reattaching {}...", name));
                }
            }
            KeyCode::Char('f') if pr_review_mode => {
                let idx = self.selected_pr_review.min(self.pending_pr_review.len() - 1);
                self.forget_pr_review(idx);
            }
            KeyCode::Char('a') if self.focus == FocusPanel::Agents => {
                if let Some(name) = self.selected_agent_name() {
                    return KeyEffect::AttachAgent(name);
                }
            }
            KeyCode::Char('y') if approval_mode => self.approve_request(),
            KeyCode::Char('n') if approval_mode => self.deny_request(),
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Up => self.move_selection_up(),
            KeyCode::Down => self.move_selection_down(),
            KeyCode::Enter => self.submit_answer(),
            KeyCode::Backspace => { self.input_text.pop(); }
            KeyCode::Char(c) => { self.input_text.push(c); }
            KeyCode::Esc => { self.input_text.clear(); }
            _ => {}
        }
        KeyEffect::None
    }

    pub fn move_selection_down(&mut self) {
        match self.focus {
            FocusPanel::Agents => {
                if !self.agents.is_empty() && self.selected_agent < self.agents.len() - 1 {
                    self.selected_agent += 1;
                }
            }
            FocusPanel::Requests => {
                if !self.pending_requests.is_empty()
                    && self.selected_request < self.pending_requests.len() - 1
                {
                    self.selected_request += 1;
                }
            }
            FocusPanel::PrReview => {
                if !self.pending_pr_review.is_empty()
                    && self.selected_pr_review < self.pending_pr_review.len() - 1
                {
                    self.selected_pr_review += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_app() -> (App, mpsc::UnboundedReceiver<TuiCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        (App::new(cmd_tx), cmd_rx)
    }

    fn agent_connected(id: &str, name: &str, role: &str) -> OrchestratorEvent {
        OrchestratorEvent::AgentConnected(AgentInfo {
            id: id.into(),
            name: name.into(),
            role: role.into(),
            workspace_path: None,
        })
    }

    fn request_received(agent_name: &str, request_id: &str, question: &str) -> OrchestratorEvent {
        OrchestratorEvent::RequestReceived {
            agent_id: "agent-1".into(),
            agent_name: agent_name.into(),
            request_id: request_id.into(),
            request_type: "user_prompt".into(),
            payload: json!({"question": question}),
        }
    }

    #[test]
    fn agent_connect_adds_to_list_and_log() {
        let (mut app, _rx) = make_app();
        app.handle_event(agent_connected("a1", "coder", "code-agent"));

        assert_eq!(app.agents.len(), 1);
        assert_eq!(app.agents[0].name, "coder");
        assert!(app.completed_log.last().unwrap().contains("coder"));
    }

    #[test]
    fn agent_disconnect_removes_and_logs() {
        let (mut app, _rx) = make_app();
        app.handle_event(agent_connected("a1", "coder", "code-agent"));
        app.handle_event(OrchestratorEvent::AgentDisconnected { id: "a1".into() });

        assert!(app.agents.is_empty());
        assert!(app.completed_log.last().unwrap().contains("disconnected"));
    }

    #[test]
    fn disconnect_adjusts_selection_index() {
        let (mut app, _rx) = make_app();
        app.handle_event(agent_connected("a1", "first", "code-agent"));
        app.handle_event(agent_connected("a2", "second", "code-agent"));
        app.focus = FocusPanel::Agents;
        app.selected_agent = 1;

        app.handle_event(OrchestratorEvent::AgentDisconnected { id: "a2".into() });
        assert_eq!(app.selected_agent, 0);
    }

    #[test]
    fn request_received_adds_to_pending_and_focuses() {
        let (mut app, _rx) = make_app();
        app.focus = FocusPanel::Agents;

        app.handle_event(request_received("coder", "r1", "What color?"));

        assert_eq!(app.pending_requests.len(), 1);
        assert_eq!(app.pending_requests[0].question, "What color?");
        assert_eq!(app.focus, FocusPanel::Requests);
        assert_eq!(app.selected_request, 0);
    }

    #[test]
    fn submit_answer_sends_command_and_clears() {
        let (mut app, mut rx) = make_app();
        app.handle_event(request_received("coder", "r1", "Color?"));
        app.input_text = "blue".into();

        app.submit_answer();

        assert!(app.pending_requests.is_empty());
        assert!(app.input_text.is_empty());
        assert!(app.completed_log.last().unwrap().contains("blue"));

        let cmd = rx.try_recv().unwrap();
        match cmd {
            TuiCommand::RespondToRequest { request_id, payload } => {
                assert_eq!(request_id, "r1");
                assert_eq!(payload["answer"], "blue");
            }
            _ => panic!("Expected RespondToRequest"),
        }
    }

    #[test]
    fn submit_answer_with_empty_input_is_noop() {
        let (mut app, mut rx) = make_app();
        app.handle_event(request_received("coder", "r1", "Color?"));

        app.submit_answer(); // input is empty

        assert_eq!(app.pending_requests.len(), 1); // still pending
        assert!(rx.try_recv().is_err()); // no command sent
    }

    #[test]
    fn submit_answer_with_no_requests_is_noop() {
        let (mut app, mut rx) = make_app();
        app.input_text = "answer".into();

        app.submit_answer();

        assert_eq!(app.input_text, "answer"); // unchanged
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn toggle_focus_switches_panels() {
        let (mut app, _rx) = make_app();
        assert_eq!(app.focus, FocusPanel::Requests);
        app.toggle_focus();
        assert_eq!(app.focus, FocusPanel::PrReview);
        app.toggle_focus();
        assert_eq!(app.focus, FocusPanel::Agents);
        app.toggle_focus();
        assert_eq!(app.focus, FocusPanel::Requests);
    }

    #[test]
    fn navigation_clamps_to_bounds() {
        let (mut app, _rx) = make_app();
        app.handle_event(agent_connected("a1", "first", "r"));
        app.handle_event(agent_connected("a2", "second", "r"));
        app.focus = FocusPanel::Agents;

        app.move_selection_up(); // already at 0
        assert_eq!(app.selected_agent, 0);

        app.move_selection_down();
        assert_eq!(app.selected_agent, 1);

        app.move_selection_down(); // at end
        assert_eq!(app.selected_agent, 1);
    }

    #[test]
    fn submit_adjusts_selection_when_removing_last_request() {
        let (mut app, _rx) = make_app();
        app.handle_event(request_received("a", "r1", "Q1"));
        app.handle_event(request_received("a", "r2", "Q2"));
        app.selected_request = 1;
        app.input_text = "answer".into();

        app.submit_answer(); // removes r2, selected was 1

        assert_eq!(app.selected_request, 0);
    }

    // --- Key dispatch tests ---

    #[test]
    fn key_q_enters_confirm_quit() {
        let (mut app, _rx) = make_app();
        use crossterm::event::KeyCode;
        let effect = app.handle_key(KeyCode::Char('q'));
        assert_eq!(effect, KeyEffect::None);
        assert_eq!(app.input_mode, InputMode::ConfirmQuit);
    }

    #[test]
    fn confirm_quit_y_returns_quit_effect() {
        let (mut app, _rx) = make_app();
        use crossterm::event::KeyCode;
        app.input_mode = InputMode::ConfirmQuit;
        let effect = app.handle_key(KeyCode::Char('y'));
        assert_eq!(effect, KeyEffect::Quit);
    }

    #[test]
    fn confirm_quit_other_key_cancels() {
        let (mut app, _rx) = make_app();
        use crossterm::event::KeyCode;
        app.input_mode = InputMode::ConfirmQuit;
        let effect = app.handle_key(KeyCode::Char('x'));
        assert_eq!(effect, KeyEffect::None);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn key_n_enters_new_agent_mode() {
        let (mut app, _rx) = make_app();
        use crossterm::event::KeyCode;
        app.handle_key(KeyCode::Char('N'));
        assert_eq!(app.input_mode, InputMode::NewAgent);
    }

    #[test]
    fn new_agent_enter_sends_start_command() {
        let (mut app, mut rx) = make_app();
        use crossterm::event::KeyCode;
        app.input_mode = InputMode::NewAgent;
        app.handle_key(KeyCode::Char('B'));
        app.handle_key(KeyCode::Char('o'));
        app.handle_key(KeyCode::Char('b'));
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);
        let cmd = rx.try_recv().unwrap();
        match cmd {
            TuiCommand::StartNewAgent { name, role } => {
                assert_eq!(name, "Bob");
                assert_eq!(role, "code-agent");
            }
            _ => panic!("Expected StartNewAgent"),
        }
    }

    #[test]
    fn new_agent_with_role() {
        let (mut app, mut rx) = make_app();
        use crossterm::event::KeyCode;
        app.input_mode = InputMode::NewAgent;
        for c in "Alice:review-agent".chars() {
            app.handle_key(KeyCode::Char(c));
        }
        app.handle_key(KeyCode::Enter);
        let cmd = rx.try_recv().unwrap();
        match cmd {
            TuiCommand::StartNewAgent { name, role } => {
                assert_eq!(name, "Alice");
                assert_eq!(role, "review-agent");
            }
            _ => panic!("Expected StartNewAgent"),
        }
    }

    #[test]
    fn new_agent_esc_cancels() {
        let (mut app, _rx) = make_app();
        use crossterm::event::KeyCode;
        app.input_mode = InputMode::NewAgent;
        app.handle_key(KeyCode::Char('B'));
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.input_text.is_empty());
    }

    #[test]
    fn key_a_returns_attach_effect() {
        let (mut app, _rx) = make_app();
        use crossterm::event::KeyCode;
        app.handle_event(agent_connected("a1", "Bob", "code-agent"));
        app.focus = FocusPanel::Agents;
        let effect = app.handle_key(KeyCode::Char('a'));
        assert_eq!(effect, KeyEffect::AttachAgent("Bob".into()));
    }

    // --- PR review pane tests ---

    fn team_pr_merged(team_id: &str, ticket_id: &str, pr_number: u64) -> OrchestratorEvent {
        OrchestratorEvent::TeamPrMerged {
            team_id: team_id.into(),
            ticket_id: ticket_id.into(),
            pr_number,
            merge_commit: Some("abc123".into()),
        }
    }

    fn team_pr_closed(team_id: &str, ticket_id: &str, pr_number: u64) -> OrchestratorEvent {
        OrchestratorEvent::TeamPrClosed {
            team_id: team_id.into(),
            ticket_id: ticket_id.into(),
            pr_number,
        }
    }

    #[test]
    fn team_pr_merged_logs_and_sends_close_command() {
        let (mut app, mut rx) = make_app();
        app.handle_event(team_pr_merged("t-foo", "ticket-1", 42));

        assert!(app.completed_log.last().unwrap().contains("ticket-1"));
        assert!(app.pending_pr_review.is_empty(), "merged PR must not add to pending_pr_review");

        let cmd = rx.try_recv().unwrap();
        match cmd {
            TuiCommand::CloseAndTeardownTeam { team_id, ticket_id, pr_number, .. } => {
                assert_eq!(team_id, "t-foo");
                assert_eq!(ticket_id, "ticket-1");
                assert_eq!(pr_number, 42);
            }
            _ => panic!("Expected CloseAndTeardownTeam, got {:?}", cmd),
        }
    }

    #[test]
    fn team_pr_closed_adds_to_pending_pr_review() {
        let (mut app, mut rx) = make_app();
        app.handle_event(team_pr_closed("t-bar", "ticket-2", 7));

        assert_eq!(app.pending_pr_review.len(), 1);
        assert_eq!(app.pending_pr_review[0].team_id, "t-bar");
        assert_eq!(app.pending_pr_review[0].pr_number, 7);
        assert!(rx.try_recv().is_err(), "no command sent on TeamPrClosed");
    }

    #[test]
    fn key_f_on_pr_review_pane_sends_forget_command() {
        let (mut app, mut rx) = make_app();
        app.handle_event(team_pr_closed("t-baz", "ticket-3", 5));
        app.focus = FocusPanel::PrReview;

        use crossterm::event::KeyCode;
        app.handle_key(KeyCode::Char('f'));

        assert!(app.pending_pr_review.is_empty(), "entry must be removed after forget");
        let cmd = rx.try_recv().unwrap();
        match cmd {
            TuiCommand::ForgetTeamPr { team_id, ticket_id, pr_number } => {
                assert_eq!(team_id, "t-baz");
                assert_eq!(ticket_id, "ticket-3");
                assert_eq!(pr_number, 5);
            }
            _ => panic!("Expected ForgetTeamPr, got {:?}", cmd),
        }
    }

}
