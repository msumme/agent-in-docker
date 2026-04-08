use std::collections::HashMap;

use crate::types::*;

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

/// Generate the shell script that auto-accepts the bypass permissions dialog
/// and sends an initial prompt to an agent in a tmux pane.
pub fn auto_accept_script(tmux_target: &str, agent_name: &str, prompt: &str) -> String {
    let prompt_file = format!("/tmp/agent-prompt-{}.txt", agent_name);
    if !prompt.is_empty() {
        let _ = std::fs::write(&prompt_file, prompt);
    }
    format!(
        r#"for i in $(seq 1 30); do sleep 2; pane=$(tmux capture-pane -t '{t}' -p 2>/dev/null); if echo "$pane" | grep -q 'Yes, I accept'; then tmux send-keys -t '{t}' Down; sleep 1; tmux send-keys -t '{t}' Enter; break; fi; if echo "$pane" | grep -q '╭─'; then break; fi; done; if [ -f '{pf}' ]; then sleep 3; tmux send-keys -t '{t}' "$(cat '{pf}')" Enter; rm -f '{pf}'; fi"#,
        t = tmux_target,
        pf = prompt_file,
    )
}

/// Manages agent lifecycles: start, stop, track status.
pub struct AgentManager {
    agents: HashMap<String, ManagedAgent>,
    tmux_session: String,
    tmux: Box<dyn TmuxOps>,
    container: Box<dyn ContainerOps>,
}

impl AgentManager {
    pub fn new(
        tmux_session: String,
        tmux: Box<dyn TmuxOps>,
        container: Box<dyn ContainerOps>,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            tmux_session,
            tmux,
            container,
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

        // Spawn background shell to auto-accept the bypass permissions dialog.
        let target = format!("{}:{}", self.tmux_session, cfg.name);
        let script = auto_accept_script(&target, &cfg.name, &cfg.prompt);
        let _ = std::process::Command::new("sh")
            .args(["-c", &format!("({}) &", script)])
            .spawn();

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
        }
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
        if let Some(agent) = self.agents.get_mut(agent_name) {
            if agent.status == AgentStatus::Working {
                agent.status = AgentStatus::Idle;
            }
        }
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
    use std::sync::Mutex;

    struct FakeTmux {
        windows: Mutex<Vec<(String, String, String)>>,
    }

    impl FakeTmux {
        fn new() -> Self {
            Self { windows: Mutex::new(Vec::new()) }
        }
    }

    impl TmuxOps for FakeTmux {
        fn create_window(&self, session: &str, name: &str, cmd: &str) -> Result<(), String> {
            self.windows.lock().unwrap().push((session.into(), name.into(), cmd.into()));
            Ok(())
        }
        fn select_window(&self, _session: &str, _name: &str) -> Result<(), String> { Ok(()) }
        fn send_keys(&self, _target: &str, _keys: &str) -> Result<(), String> { Ok(()) }
        fn capture_pane(&self, _target: &str) -> Result<String, String> { Ok(String::new()) }
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

    fn make_manager() -> AgentManager {
        AgentManager::new(
            "test-session".into(),
            Box::new(FakeTmux::new()),
            Box::new(FakeContainer),
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
}
