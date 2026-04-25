use std::collections::{HashMap, VecDeque};

use crate::types::*;

/// A message waiting to be delivered to an agent that is currently working.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub from_label: String,
    pub content: String,
}

/// Abstraction over tmux operations. Injectable for testing.
pub trait TmuxOps: Send + Sync {
    fn create_window(&self, session: &str, window_name: &str, command: &str) -> Result<(), String>;
    fn select_window(&self, session: &str, window_name: &str) -> Result<(), String>;
    fn send_keys(&self, target: &str, keys: &str) -> Result<(), String>;
    fn capture_pane(&self, target: &str) -> Result<String, String>;
}

/// Abstraction over podman operations. Injectable for testing.
pub trait ContainerOps: Send + Sync {
    fn build_run_command(&self, cfg: &StartAgentPayload) -> String;
    fn is_running(&self, container_name: &str) -> bool;
    fn stop(&self, container_name: &str) -> Result<(), String>;
    fn ensure_network(&self, network_name: &str);
}

/// Abstraction over shell/filesystem operations needed by agent startup.
/// Injectable for testing — prevents tests from writing to /tmp and spawning processes.
pub trait ShellOps: Send + Sync {
    fn write_prompt_file(&self, agent_name: &str, prompt: &str) -> Result<(), String>;
    fn spawn_background_script(&self, script: &str) -> Result<(), String>;
}

/// Real implementations using std::process::Command.
pub struct RealTmuxOps;

impl TmuxOps for RealTmuxOps {
    fn create_window(&self, session: &str, window_name: &str, command: &str) -> Result<(), String> {
        let target = format!("{}:", session); // trailing colon = next available index
        let status = std::process::Command::new("tmux")
            .args(["new-window", "-t", &target, "-n", window_name, command])
            .status()
            .map_err(|e| format!("tmux new-window failed: {}", e))?;
        if status.success() { Ok(()) } else { Err("tmux new-window returned non-zero".into()) }
    }

    fn select_window(&self, session: &str, window_name: &str) -> Result<(), String> {
        let target = format!("{}:{}", session, window_name);
        let _ = std::process::Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status();
        Ok(())
    }

    fn send_keys(&self, target: &str, keys: &str) -> Result<(), String> {
        let _ = std::process::Command::new("tmux")
            .args(["send-keys", "-t", target, keys])
            .status();
        Ok(())
    }

    fn capture_pane(&self, target: &str) -> Result<String, String> {
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-t", target, "-p"])
            .output()
            .map_err(|e| format!("tmux capture-pane failed: {}", e))?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

pub struct RealContainerOps;

impl ContainerOps for RealContainerOps {
    fn build_run_command(&self, cfg: &StartAgentPayload) -> String {
        let args = cfg.container_run_args();
        format!("podman run -it {}", args.join(" "))
    }

    fn is_running(&self, container_name: &str) -> bool {
        std::process::Command::new("podman")
            .args(["inspect", "--format", "{{.State.Running}}", container_name])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false)
    }

    fn stop(&self, container_name: &str) -> Result<(), String> {
        let status = std::process::Command::new("podman")
            .args(["stop", container_name])
            .status()
            .map_err(|e| format!("podman stop failed: {}", e))?;
        if status.success() { Ok(()) } else { Err("podman stop returned non-zero".into()) }
    }

    fn ensure_network(&self, network_name: &str) {
        let _ = std::process::Command::new("podman")
            .args(["network", "create", network_name])
            .output();
    }
}

pub struct RealShellOps;

impl ShellOps for RealShellOps {
    fn write_prompt_file(&self, agent_name: &str, prompt: &str) -> Result<(), String> {
        let path = format!("/tmp/agent-prompt-{}.txt", agent_name);
        std::fs::write(&path, prompt).map_err(|e| format!("write prompt file: {}", e))
    }

    fn spawn_background_script(&self, script: &str) -> Result<(), String> {
        std::process::Command::new("sh")
            .args(["-c", &format!("({}) &", script)])
            .spawn()
            .map_err(|e| format!("spawn script: {}", e))?;
        Ok(())
    }
}

/// Generate the shell script that auto-accepts the bypass permissions dialog
/// and sends an initial prompt to an agent in a tmux pane.
pub fn auto_accept_script(tmux_target: &str, agent_name: &str) -> String {
    let prompt_file = format!("/tmp/agent-prompt-{}.txt", agent_name);
    format!(
        r#"for i in $(seq 1 30); do sleep 2; pane=$(tmux capture-pane -t '{t}' -p 2>/dev/null); if echo "$pane" | grep -q 'Yes, I accept'; then tmux send-keys -t '{t}' Down; sleep 1; tmux send-keys -t '{t}' Enter; break; fi; if echo "$pane" | grep -q '╭─'; then break; fi; done; if [ -f '{pf}' ]; then sleep 3; tmux send-keys -t '{t}' "$(cat '{pf}')" Enter; rm -f '{pf}'; fi"#,
        t = tmux_target,
        pf = prompt_file,
    )
}

/// Manages agent lifecycles: start, stop, track status, route messages.
pub struct AgentManager {
    agents: HashMap<String, ManagedAgent>,
    /// Per-agent queues of messages received while the agent was Working.
    /// Flushed to tmux when the agent next goes Idle.
    mailboxes: HashMap<String, VecDeque<QueuedMessage>>,
    tmux_session: String,
    tmux: Box<dyn TmuxOps>,
    container: Box<dyn ContainerOps>,
    shell: Box<dyn ShellOps>,
}

impl AgentManager {
    pub fn new(
        tmux_session: String,
        tmux: Box<dyn TmuxOps>,
        container: Box<dyn ContainerOps>,
        shell: Box<dyn ShellOps>,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            mailboxes: HashMap::new(),
            tmux_session,
            tmux,
            container,
            shell,
        }
    }

    pub fn start_agent(&mut self, cfg: &StartAgentPayload) -> Result<ManagedAgent, String> {
        if self.agents.contains_key(&cfg.name) {
            let existing = &self.agents[&cfg.name];
            if existing.status != AgentStatus::Exited {
                return Err(format!("Agent '{}' is already running", cfg.name));
            }
        }

        self.container.ensure_network(&cfg.network_name);
        let podman_cmd = self.container.build_run_command(cfg);
        let window_cmd = format!("{}; echo '[Agent exited. Press Enter to close.]'; read", podman_cmd);

        // Create window FIRST -- if this fails, don't insert the agent
        self.tmux.create_window(&self.tmux_session, &cfg.name, &window_cmd)?;

        // Write prompt file and spawn auto-accept script via injected ShellOps.
        if !cfg.prompt.is_empty() {
            let _ = self.shell.write_prompt_file(&cfg.name, &cfg.prompt);
        }
        let target = format!("{}:{}", self.tmux_session, cfg.name);
        let script = auto_accept_script(&target, &cfg.name);
        let _ = self.shell.spawn_background_script(&script);

        let agent = ManagedAgent {
            name: cfg.name.clone(),
            role: cfg.role.clone(),
            mode: cfg.mode.clone(),
            status: AgentStatus::Starting,
            tmux_window: cfg.name.clone(),
            container_name: cfg.name.clone(),
            project_path: cfg.project_path.clone(),
            prompt: cfg.prompt.clone(),
            ws_agent_id: None,
            last_activity: "starting".into(),
        };

        self.agents.insert(cfg.name.clone(), agent.clone());
        Ok(agent)
    }

    pub fn agent_registered(&mut self, agent_name: &str, ws_agent_id: &str) {
        if let Some(agent) = self.agents.get_mut(agent_name) {
            agent.ws_agent_id = Some(ws_agent_id.to_string());
            agent.status = AgentStatus::Connected;
            agent.last_activity = "connected".into();
            return;
        }
        // CLI-spawned agents bypass start_agent, so the manager has no entry.
        // Without one, deliver_agent_message would fail with "agent not found"
        // and inter-agent messaging would silently break. Synthesize a minimal
        // entry using conventions the CLI already follows: tmux window name and
        // container name both equal the agent name.
        self.agents.insert(
            agent_name.to_string(),
            ManagedAgent {
                name: agent_name.to_string(),
                role: String::new(),
                mode: "long-running".into(),
                status: AgentStatus::Connected,
                tmux_window: agent_name.to_string(),
                container_name: agent_name.to_string(),
                project_path: String::new(),
                prompt: String::new(),
                ws_agent_id: Some(ws_agent_id.to_string()),
                last_activity: "connected".into(),
            },
        );
    }

    pub fn agent_disconnected(&mut self, ws_agent_id: &str) {
        for agent in self.agents.values_mut() {
            if agent.ws_agent_id.as_deref() == Some(ws_agent_id) {
                agent.status = AgentStatus::Exited;
                agent.ws_agent_id = None;
                agent.last_activity = "exited".into();
            }
        }
    }

    pub fn agent_working(&mut self, agent_name: &str, activity: &str) {
        if let Some(agent) = self.agents.get_mut(agent_name) {
            agent.status = AgentStatus::Working;
            agent.last_activity = activity.to_string();
        }
    }

    pub fn agent_idle(&mut self, agent_name: &str) {
        let transitioned = if let Some(agent) = self.agents.get_mut(agent_name) {
            if agent.status == AgentStatus::Working {
                agent.status = AgentStatus::Idle;
                true
            } else {
                false
            }
        } else {
            false
        };
        if transitioned {
            self.flush_mailbox(agent_name);
        }
    }

    /// Deliver an agent-to-agent message. If the recipient is Working, queue
    /// it; it will be flushed when the recipient next goes Idle. Otherwise
    /// deliver immediately via tmux.
    ///
    /// `to_ref` accepts either the agent's name or its WS id — callers
    /// (the MCP `message_agent` tool) pass whatever the LLM produces, which
    /// is usually the id surfaced by `list_agents`.
    pub fn deliver_agent_message(
        &mut self,
        to_ref: &str,
        from_label: &str,
        content: &str,
    ) -> Result<(), String> {
        let agent = self
            .agents
            .get(to_ref)
            .or_else(|| {
                self.agents
                    .values()
                    .find(|a| a.ws_agent_id.as_deref() == Some(to_ref))
            })
            .ok_or_else(|| format!("Agent '{}' not found", to_ref))?;
        let to_name = agent.name.clone();
        let status = agent.status.clone();
        let window = agent.tmux_window.clone();
        let deliverable_now = matches!(
            status,
            AgentStatus::Idle | AgentStatus::Connected | AgentStatus::Starting
        );
        if deliverable_now {
            self.send_message_to_window(&window, from_label, content)
        } else {
            self.mailboxes
                .entry(to_name.to_string())
                .or_default()
                .push_back(QueuedMessage {
                    from_label: from_label.to_string(),
                    content: content.to_string(),
                });
            Ok(())
        }
    }

    /// Deliver a user (TUI) message immediately, bypassing the working-state
    /// check. User intent trumps agent working state.
    pub fn deliver_user_message(&mut self, to_name: &str, content: &str) -> Result<(), String> {
        let agent = self
            .agents
            .get(to_name)
            .ok_or_else(|| format!("Agent '{}' not found", to_name))?;
        let window = agent.tmux_window.clone();
        self.send_message_to_window(&window, "user", content)
    }

    /// Number of queued messages for an agent. Exposed for observability/tests.
    pub fn mailbox_len(&self, agent_name: &str) -> usize {
        self.mailboxes
            .get(agent_name)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    fn flush_mailbox(&mut self, agent_name: &str) {
        let window = match self.agents.get(agent_name) {
            Some(a) => a.tmux_window.clone(),
            None => return,
        };
        let drained: Vec<QueuedMessage> = match self.mailboxes.get_mut(agent_name) {
            Some(mb) => mb.drain(..).collect(),
            None => return,
        };
        for msg in drained {
            let _ = self.send_message_to_window(&window, &msg.from_label, &msg.content);
        }
    }

    fn send_message_to_window(
        &self,
        window: &str,
        from_label: &str,
        content: &str,
    ) -> Result<(), String> {
        let target = format!("{}:{}", self.tmux_session, window);
        let formatted = format!("[from {}]: {}", from_label, content);
        self.tmux.send_keys(&target, &formatted)?;
        self.tmux.send_keys(&target, "Enter")?;
        Ok(())
    }

    /// Reattach an orphaned container to a new tmux window.
    /// Use when the container is running but its tmux window was lost.
    pub fn reattach_agent(&mut self, name: &str) -> Result<(), String> {
        let agent = self.agents.get(name).ok_or(format!("Agent '{}' not found", name))?;

        if !self.container.is_running(&agent.container_name) {
            return Err(format!("Container '{}' is not running", name));
        }

        // Create a new tmux window that attaches to the running container
        let cmd = format!("podman attach {}", agent.container_name);
        self.tmux.create_window(&self.tmux_session, name, &cmd)?;

        if let Some(agent) = self.agents.get_mut(name) {
            agent.status = AgentStatus::Connected;
            agent.last_activity = "reattached".into();
        }
        Ok(())
    }

    pub fn stop_agent(&mut self, name: &str) -> Result<(), String> {
        let _ = self.container.stop(name);
        if let Some(agent) = self.agents.get_mut(name) {
            agent.status = AgentStatus::Exited;
            agent.last_activity = "stopped by user".into();
        }
        Ok(())
    }

    pub fn get_agent(&self, name: &str) -> Option<&ManagedAgent> {
        self.agents.get(name)
    }

    pub fn get_agent_by_ws_id(&self, ws_id: &str) -> Option<&ManagedAgent> {
        self.agents.values().find(|a| a.ws_agent_id.as_deref() == Some(ws_id))
    }

    pub fn list_agents(&self) -> Vec<ManagedAgent> {
        self.agents.values().cloned().collect()
    }

    pub fn attach_to_agent(&self, name: &str) -> Result<(), String> {
        self.tmux.select_window(&self.tmux_session, name)
    }

    pub fn tmux_session_name(&self) -> &str {
        &self.tmux_session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct FakeTmux {
        windows: Mutex<Vec<(String, String, String)>>,
        sent_keys: Mutex<Vec<(String, String)>>,
    }

    impl FakeTmux {
        fn new() -> Self {
            Self {
                windows: Mutex::new(Vec::new()),
                sent_keys: Mutex::new(Vec::new()),
            }
        }
    }

    impl TmuxOps for FakeTmux {
        fn create_window(&self, session: &str, name: &str, cmd: &str) -> Result<(), String> {
            self.windows.lock().unwrap().push((session.into(), name.into(), cmd.into()));
            Ok(())
        }
        fn select_window(&self, _session: &str, _name: &str) -> Result<(), String> { Ok(()) }
        fn send_keys(&self, target: &str, keys: &str) -> Result<(), String> {
            self.sent_keys.lock().unwrap().push((target.into(), keys.into()));
            Ok(())
        }
        fn capture_pane(&self, _target: &str) -> Result<String, String> { Ok(String::new()) }
    }

    struct FakeTmuxWrapper(Arc<FakeTmux>);
    impl TmuxOps for FakeTmuxWrapper {
        fn create_window(&self, session: &str, name: &str, cmd: &str) -> Result<(), String> {
            self.0.create_window(session, name, cmd)
        }
        fn select_window(&self, session: &str, name: &str) -> Result<(), String> {
            self.0.select_window(session, name)
        }
        fn send_keys(&self, target: &str, keys: &str) -> Result<(), String> {
            self.0.send_keys(target, keys)
        }
        fn capture_pane(&self, target: &str) -> Result<String, String> {
            self.0.capture_pane(target)
        }
    }

    struct FakeContainer;
    impl ContainerOps for FakeContainer {
        fn build_run_command(&self, cfg: &StartAgentPayload) -> String {
            format!("podman run --name {} {}", cfg.name, cfg.image_name)
        }
        fn is_running(&self, _name: &str) -> bool { true }
        fn stop(&self, _name: &str) -> Result<(), String> { Ok(()) }
        fn ensure_network(&self, _name: &str) {}
    }

    struct FakeShell {
        prompt_files: Mutex<Vec<(String, String)>>,
        scripts: Mutex<Vec<String>>,
    }

    impl FakeShell {
        fn new() -> Self {
            Self {
                prompt_files: Mutex::new(Vec::new()),
                scripts: Mutex::new(Vec::new()),
            }
        }
    }

    impl ShellOps for FakeShell {
        fn write_prompt_file(&self, agent_name: &str, prompt: &str) -> Result<(), String> {
            self.prompt_files.lock().unwrap().push((agent_name.into(), prompt.into()));
            Ok(())
        }
        fn spawn_background_script(&self, script: &str) -> Result<(), String> {
            self.scripts.lock().unwrap().push(script.into());
            Ok(())
        }
    }

    fn make_shell() -> Arc<FakeShell> {
        Arc::new(FakeShell::new())
    }

    fn make_manager_with_shell(shell: Arc<FakeShell>) -> AgentManager {
        AgentManager::new(
            "test-session".into(),
            Box::new(FakeTmux::new()),
            Box::new(FakeContainer),
            Box::new(FakeShellWrapper(shell)),
        )
    }

    // Wrapper so Arc<FakeShell> can be shared between test and manager
    struct FakeShellWrapper(Arc<FakeShell>);
    impl ShellOps for FakeShellWrapper {
        fn write_prompt_file(&self, agent_name: &str, prompt: &str) -> Result<(), String> {
            self.0.write_prompt_file(agent_name, prompt)
        }
        fn spawn_background_script(&self, script: &str) -> Result<(), String> {
            self.0.spawn_background_script(script)
        }
    }

    fn make_manager() -> AgentManager {
        AgentManager::new(
            "test-session".into(),
            Box::new(FakeTmux::new()),
            Box::new(FakeContainer),
            Box::new(FakeShell::new()),
        )
    }

    fn make_payload(name: &str) -> StartAgentPayload {
        StartAgentPayload {
            name: name.into(),
            role: "code-agent".into(),
            mode: "long-running".into(),
            project_path: "/tmp/project".into(),
            prompt: "hello".into(),
            agent_dir: "/tmp/agent".into(),
            role_memory_dir: "/tmp/role-memory".into(),
            role_prompt: String::new(),
            seed_credentials: "/tmp/creds.json".into(),
            image_name: "agent-in-docker".into(),
            network_name: "agent-net".into(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            dolt_port: None,
        }
    }

    #[test]
    fn start_agent_creates_managed_agent() {
        let mut mgr = make_manager();
        let agent = mgr.start_agent(&make_payload("Alice")).unwrap();
        assert_eq!(agent.name, "Alice");
        assert_eq!(agent.status, AgentStatus::Starting);
        assert_eq!(mgr.list_agents().len(), 1);
    }

    #[test]
    fn start_duplicate_agent_fails() {
        let mut mgr = make_manager();
        mgr.start_agent(&make_payload("Alice")).unwrap();
        let result = mgr.start_agent(&make_payload("Alice"));
        assert!(result.is_err());
    }

    #[test]
    fn start_exited_agent_succeeds() {
        let mut mgr = make_manager();
        mgr.start_agent(&make_payload("Alice")).unwrap();
        mgr.agent_registered("Alice", "ws-1");
        mgr.agent_disconnected("ws-1");
        assert_eq!(mgr.get_agent("Alice").unwrap().status, AgentStatus::Exited);
        let agent = mgr.start_agent(&make_payload("Alice")).unwrap();
        assert_eq!(agent.status, AgentStatus::Starting);
    }

    #[test]
    fn agent_lifecycle_transitions() {
        let mut mgr = make_manager();
        mgr.start_agent(&make_payload("Bob")).unwrap();
        assert_eq!(mgr.get_agent("Bob").unwrap().status, AgentStatus::Starting);

        mgr.agent_registered("Bob", "ws-1");
        assert_eq!(mgr.get_agent("Bob").unwrap().status, AgentStatus::Connected);
        assert_eq!(mgr.get_agent("Bob").unwrap().ws_agent_id.as_deref(), Some("ws-1"));

        mgr.agent_working("Bob", "ask_user: What color?");
        assert_eq!(mgr.get_agent("Bob").unwrap().status, AgentStatus::Working);
        assert_eq!(mgr.get_agent("Bob").unwrap().last_activity, "ask_user: What color?");

        mgr.agent_idle("Bob");
        assert_eq!(mgr.get_agent("Bob").unwrap().status, AgentStatus::Idle);

        mgr.agent_disconnected("ws-1");
        assert_eq!(mgr.get_agent("Bob").unwrap().status, AgentStatus::Exited);
    }

    #[test]
    fn agent_registered_creates_entry_for_cli_spawned_agent() {
        // CLI-spawned agents bypass start_agent. Without auto-creation here,
        // deliver_agent_message would fail with "agent not found" and
        // inter-agent messaging would silently break.
        let mut mgr = make_manager();
        mgr.agent_registered("cli-spawned", "ws-7");

        let found = mgr.get_agent("cli-spawned").expect("entry must be created");
        assert_eq!(found.status, AgentStatus::Connected);
        assert_eq!(found.ws_agent_id.as_deref(), Some("ws-7"));
        assert_eq!(found.tmux_window, "cli-spawned");
        assert_eq!(found.container_name, "cli-spawned");

        // And messages routed by name now reach tmux instead of erroring.
        mgr.deliver_agent_message("cli-spawned", "max", "hello")
            .expect("delivery should succeed");
    }

    #[test]
    fn get_agent_by_ws_id() {
        let mut mgr = make_manager();
        mgr.start_agent(&make_payload("Alice")).unwrap();
        mgr.agent_registered("Alice", "ws-42");

        let found = mgr.get_agent_by_ws_id("ws-42");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "Alice");

        assert!(mgr.get_agent_by_ws_id("ws-999").is_none());
    }

    #[test]
    fn list_agents_returns_all() {
        let mut mgr = make_manager();
        mgr.start_agent(&make_payload("Alice")).unwrap();
        mgr.start_agent(&make_payload("Bob")).unwrap();
        assert_eq!(mgr.list_agents().len(), 2);
    }

    #[test]
    fn stop_agent_sets_exited() {
        let mut mgr = make_manager();
        mgr.start_agent(&make_payload("Alice")).unwrap();
        mgr.agent_registered("Alice", "ws-1");
        mgr.stop_agent("Alice").unwrap();
        assert_eq!(mgr.get_agent("Alice").unwrap().status, AgentStatus::Exited);
    }

    #[test]
    fn reattach_running_agent_sets_connected() {
        let mut mgr = make_manager();
        mgr.start_agent(&make_payload("Alice")).unwrap();
        mgr.agent_registered("Alice", "ws-1");
        mgr.agent_disconnected("ws-1"); // simulate disconnect
        // FakeContainer.is_running returns true
        mgr.reattach_agent("Alice").unwrap();
        assert_eq!(mgr.get_agent("Alice").unwrap().status, AgentStatus::Connected);
    }

    #[test]
    fn reattach_unknown_agent_fails() {
        let mut mgr = make_manager();
        assert!(mgr.reattach_agent("Nobody").is_err());
    }

    #[test]
    fn start_agent_writes_prompt_file() {
        let shell = make_shell();
        let mut mgr = make_manager_with_shell(shell.clone());
        mgr.start_agent(&make_payload("Alice")).unwrap();

        let files = shell.prompt_files.lock().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], ("Alice".into(), "hello".into()));
    }

    #[test]
    fn start_agent_spawns_accept_script() {
        let shell = make_shell();
        let mut mgr = make_manager_with_shell(shell.clone());
        mgr.start_agent(&make_payload("Bob")).unwrap();

        let scripts = shell.scripts.lock().unwrap();
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("test-session:Bob"));
    }

    fn make_manager_with_tmux(tmux: Arc<FakeTmux>) -> AgentManager {
        AgentManager::new(
            "test-session".into(),
            Box::new(FakeTmuxWrapper(tmux)),
            Box::new(FakeContainer),
            Box::new(FakeShell::new()),
        )
    }

    #[test]
    fn agent_message_delivers_immediately_when_idle() {
        let tmux = Arc::new(FakeTmux::new());
        let mut mgr = make_manager_with_tmux(tmux.clone());
        mgr.start_agent(&make_payload("alice")).unwrap();
        mgr.agent_registered("alice", "ws-1");
        mgr.agent_working("alice", "");
        mgr.agent_idle("alice");

        mgr.deliver_agent_message("alice", "producer", "pass 1 ready sha=abc")
            .unwrap();
        assert_eq!(mgr.mailbox_len("alice"), 0);

        let sent = tmux.sent_keys.lock().unwrap();
        assert!(sent
            .iter()
            .any(|(t, k)| t == "test-session:alice" && k == "[from producer]: pass 1 ready sha=abc"));
        assert!(sent.iter().any(|(t, k)| t == "test-session:alice" && k == "Enter"));
    }

    #[test]
    fn agent_message_queues_when_working() {
        let tmux = Arc::new(FakeTmux::new());
        let mut mgr = make_manager_with_tmux(tmux.clone());
        mgr.start_agent(&make_payload("alice")).unwrap();
        mgr.agent_registered("alice", "ws-1");
        mgr.agent_working("alice", "thinking");

        mgr.deliver_agent_message("alice", "producer", "msg1").unwrap();
        mgr.deliver_agent_message("alice", "cleaner", "msg2").unwrap();

        assert_eq!(mgr.mailbox_len("alice"), 2);
        // Nothing sent to tmux while working.
        let sent_before = tmux.sent_keys.lock().unwrap().len();
        assert_eq!(sent_before, 0);
    }

    #[test]
    fn idle_transition_flushes_mailbox_in_order() {
        let tmux = Arc::new(FakeTmux::new());
        let mut mgr = make_manager_with_tmux(tmux.clone());
        mgr.start_agent(&make_payload("alice")).unwrap();
        mgr.agent_registered("alice", "ws-1");
        mgr.agent_working("alice", "thinking");
        mgr.deliver_agent_message("alice", "producer", "first").unwrap();
        mgr.deliver_agent_message("alice", "cleaner", "second").unwrap();

        mgr.agent_idle("alice");

        assert_eq!(mgr.mailbox_len("alice"), 0);
        let sent = tmux.sent_keys.lock().unwrap();
        let bodies: Vec<&String> = sent
            .iter()
            .filter(|(_, k)| k != "Enter")
            .map(|(_, k)| k)
            .collect();
        assert_eq!(
            bodies,
            vec![
                &"[from producer]: first".to_string(),
                &"[from cleaner]: second".to_string(),
            ]
        );
    }

    #[test]
    fn user_message_bypasses_working_state() {
        let tmux = Arc::new(FakeTmux::new());
        let mut mgr = make_manager_with_tmux(tmux.clone());
        mgr.start_agent(&make_payload("alice")).unwrap();
        mgr.agent_registered("alice", "ws-1");
        mgr.agent_working("alice", "thinking");

        mgr.deliver_user_message("alice", "stop and do this").unwrap();

        assert_eq!(mgr.mailbox_len("alice"), 0);
        let sent = tmux.sent_keys.lock().unwrap();
        assert!(sent
            .iter()
            .any(|(_, k)| k == "[from user]: stop and do this"));
    }

    #[test]
    fn deliver_to_unknown_agent_errors() {
        let mut mgr = make_manager();
        assert!(mgr.deliver_agent_message("ghost", "producer", "hi").is_err());
        assert!(mgr.deliver_user_message("ghost", "hi").is_err());
    }

    #[test]
    fn agent_idle_when_already_idle_does_not_double_flush() {
        let tmux = Arc::new(FakeTmux::new());
        let mut mgr = make_manager_with_tmux(tmux.clone());
        mgr.start_agent(&make_payload("alice")).unwrap();
        mgr.agent_registered("alice", "ws-1");
        mgr.agent_working("alice", "");
        mgr.deliver_agent_message("alice", "producer", "msg").unwrap();
        mgr.agent_idle("alice");
        let after_first = tmux.sent_keys.lock().unwrap().len();
        // Calling agent_idle again with status already Idle must be a no-op.
        mgr.agent_idle("alice");
        assert_eq!(tmux.sent_keys.lock().unwrap().len(), after_first);
    }

    #[test]
    fn start_agent_skips_prompt_file_when_empty() {
        let shell = make_shell();
        let mut mgr = make_manager_with_shell(shell.clone());
        let mut payload = make_payload("Eve");
        payload.prompt = String::new();
        mgr.start_agent(&payload).unwrap();

        let files = shell.prompt_files.lock().unwrap();
        assert!(files.is_empty());
    }
}
