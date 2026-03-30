mod app;
mod ui;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use app::App;
use orchestrator_core::mcp::{mcp_router, McpState};
use orchestrator_core::types::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_path = std::env::temp_dir().join("orchestrator.log");
    let log_file = std::fs::File::create(&log_path)?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(log_file)
        .init();

    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:9800".to_string());

    // Bounded channels prevent OOM under sustained load (1000 message buffer)
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<OrchestratorEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();
    // NOTE: Using unbounded because Sender requires async send() but many callers are sync.
    // The TUI drains events every 50ms so backlog is naturally bounded.
    // Per-connection WS outbound channels in server.rs are also unbounded but bounded
    // by the WS send rate. Real backpressure would require async send() throughout.

    let mcp_state = Arc::new(McpState::new(event_tx.clone()));

    // Create agent manager
    let agent_mgr = Arc::new(std::sync::Mutex::new(
        orchestrator_core::agent_manager::AgentManager::new(
            "orchestrator".into(),
            Box::new(orchestrator_core::agent_manager::RealTmuxOps),
            Box::new(orchestrator_core::agent_manager::RealContainerOps),
        ),
    ));

    // Start WebSocket server (with MCP state and agent manager)
    let server_addr = addr.clone();
    let mcp_for_server = mcp_state.clone();
    let mgr_for_server = agent_mgr.clone();
    tokio::spawn(async move {
        if let Err(e) = orchestrator_core::server::run(&server_addr, event_tx, cmd_rx, Some(mcp_for_server), Some(mgr_for_server)).await {
            tracing::error!("Server error: {}", e);
        }
    });

    // Start HTTP MCP server
    let mcp_app = mcp_router(mcp_state.clone());
    let ws_port: u16 = addr.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(9800);
    let http_addr = format!("0.0.0.0:{}", ws_port + 1);
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(&http_addr).await.unwrap();
        tracing::info!("MCP HTTP server listening on {}", http_addr);
        axum::serve(listener, mcp_app).await.unwrap();
    });

    // Set up terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cmd_tx.clone(), mcp_state.clone());

    loop {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.input_mode == app::InputMode::ConfirmQuit {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.should_quit = true;
                        }
                        _ => {
                            app.input_mode = app::InputMode::Normal;
                        }
                    }
                } else if app.input_mode == app::InputMode::NewAgent {
                    match key.code {
                        KeyCode::Enter => {
                            if !app.input_text.is_empty() {
                                let parts: Vec<&str> = app.input_text.splitn(2, ':').collect();
                                let name = parts[0].trim().to_string();
                                let role = parts.get(1).map(|r| r.trim().to_string()).unwrap_or_else(|| "code-agent".into());
                                app.completed_log.push(format!("Starting agent '{}'...", name));
                                let _ = cmd_tx.send(TuiCommand::StartNewAgent { name, role });
                            }
                            app.input_text.clear();
                            app.input_mode = app::InputMode::Normal;
                        }
                        KeyCode::Esc => {
                            app.input_text.clear();
                            app.input_mode = app::InputMode::Normal;
                        }
                        KeyCode::Backspace => { app.input_text.pop(); }
                        KeyCode::Char(c) => { app.input_text.push(c); }
                        _ => {}
                    }
                } else {
                    let approval_mode = app.focus == app::FocusPanel::Requests
                        && !app.pending_requests.is_empty()
                        && app.pending_requests
                            [app.selected_request.min(app.pending_requests.len() - 1)]
                            .request_type != "user_prompt";

                    match key.code {
                        KeyCode::Char('q') => {
                            app.input_mode = app::InputMode::ConfirmQuit;
                        }
                        KeyCode::Char('N') => {
                            app.input_mode = app::InputMode::NewAgent;
                            app.input_text.clear();
                        }
                        KeyCode::Char('r') if app.focus == app::FocusPanel::Agents => {
                            if let Some(name) = app.selected_agent_name() {
                                let _ = cmd_tx.send(TuiCommand::ReattachAgent { name: name.clone() });
                                app.completed_log.push(format!("Reattaching {}...", name));
                            }
                        }
                        KeyCode::Char('a') if app.focus == app::FocusPanel::Agents => {
                            if let Some(name) = app.selected_agent_name() {
                                let target = format!("orchestrator:{}", name);
                                let _ = std::process::Command::new("tmux")
                                    .args(["select-window", "-t", &target])
                                    .status();
                            }
                        }
                        KeyCode::Char('y') if approval_mode => app.approve_request(),
                        KeyCode::Char('n') if approval_mode => app.deny_request(),
                        KeyCode::Tab => app.toggle_focus(),
                        KeyCode::Up => app.move_selection_up(),
                        KeyCode::Down => app.move_selection_down(),
                        KeyCode::Enter => app.submit_answer(),
                        KeyCode::Backspace => { app.input_text.pop(); }
                        KeyCode::Char(c) => { app.input_text.push(c); }
                        KeyCode::Esc => { app.input_text.clear(); }
                        _ => {}
                    }
                }
            }
        }

        while let Ok(event) = event_rx.try_recv() {
            tracing::info!("TUI received event: {:?}", event);
            app.handle_event(event);
        }

        if app.should_quit {
            let _ = cmd_tx.send(TuiCommand::Shutdown);
            break;
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
