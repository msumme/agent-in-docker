use std::sync::Arc;

use orchestrator_core::mcp::McpState;
use orchestrator_core::types::*;
use serde_json::json;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Agents,
    Requests,
}

pub struct PendingRequest {
    pub agent_name: String,
    pub request_id: String,
    pub request_type: String,
    pub question: String,
}

pub struct App {
    pub agents: Vec<AgentInfo>,
    pub pending_requests: Vec<PendingRequest>,
    pub completed_log: Vec<String>,
    pub focus: FocusPanel,
    pub selected_agent: usize,
    pub selected_request: usize,
    pub input_text: String,
    pub should_quit: bool,
    pub cmd_tx: mpsc::UnboundedSender<TuiCommand>,
    mcp_state: Arc<McpState>,
}

impl App {
    pub fn new(cmd_tx: mpsc::UnboundedSender<TuiCommand>, mcp_state: Arc<McpState>) -> Self {
        Self {
            agents: Vec::new(),
            pending_requests: Vec::new(),
            completed_log: Vec::new(),
            focus: FocusPanel::Requests,
            selected_agent: 0,
            selected_request: 0,
            input_text: String::new(),
            should_quit: false,
            cmd_tx,
            mcp_state,
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
        }
    }

    /// Try to resolve an MCP pending request. Non-blocking.
    fn resolve_mcp(&self, request_id: &str, payload: serde_json::Value) {
        let mut pending = self.mcp_state.pending.lock().unwrap();
        if let Some(sender) = pending.remove(request_id) {
            let _ = sender.send(payload);
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
                self.resolve_mcp(&req.request_id, payload.clone());
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
            "[{}] {} {} -> APPROVED",
            req.agent_name, req.request_type, req.question
        ));

        // For MCP requests, execute the action and resolve directly
        if req.request_type == "file_read" {
            let payload = match orchestrator_core::handlers::file_read::read_file(&req.question) {
                Ok(content) => json!({"content": content}),
                Err(e) => json!({"code": "READ_FAILED", "message": e}),
            };
            self.resolve_mcp(&req.request_id, payload);
        }

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
        self.resolve_mcp(&req.request_id, json!({"code": "PERMISSION_DENIED", "message": "Denied by user"}));
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
            FocusPanel::Requests => FocusPanel::Agents,
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
        }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_app() -> (App, mpsc::UnboundedReceiver<TuiCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, _) = mpsc::unbounded_channel();
        let mcp_state = Arc::new(McpState::new(event_tx));
        (App::new(cmd_tx, mcp_state), cmd_rx)
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
}
