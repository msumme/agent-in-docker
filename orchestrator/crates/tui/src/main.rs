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
    let log_file = std::fs::File::create("/tmp/orchestrator.log")?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(log_file)
        .init();

    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:9800".to_string());

    // Channels between core server and TUI
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<OrchestratorEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();

    // MCP HTTP state (shared with TUI for resolving pending requests)
    let mcp_state = Arc::new(McpState::new(event_tx.clone()));

    // Start WebSocket server (with MCP state for agent registry)
    let server_addr = addr.clone();
    let mcp_for_server = mcp_state.clone();
    tokio::spawn(async move {
        if let Err(e) = orchestrator_core::server::run(&server_addr, event_tx, cmd_rx, Some(mcp_for_server)).await {
            tracing::error!("Server error: {}", e);
        }
    });

    // Start HTTP MCP server on port 9801 (separate from WS on 9800)
    let mcp_app = mcp_router(mcp_state.clone());
    let http_addr = addr.replace(":9800", ":9801").replace(":0", ":0");
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

    // Main loop
    loop {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let approval_mode = app.focus == app::FocusPanel::Requests
                    && !app.pending_requests.is_empty()
                    && app.pending_requests
                        [app.selected_request.min(app.pending_requests.len() - 1)]
                        .request_type != "user_prompt";

                match key.code {
                    KeyCode::Char('q') if app.pending_requests.is_empty() => {
                        app.should_quit = true;
                    }
                    KeyCode::Char('a') if app.focus == app::FocusPanel::Agents => {
                        if let Some(name) = app.selected_agent_name() {
                            // Leave TUI, attach to agent's tmux window
                            disable_raw_mode()?;
                            io::stdout().execute(LeaveAlternateScreen)?;
                            let target = format!("agents:{}", name);
                            let _ = std::process::Command::new("tmux")
                                .args(["select-window", "-t", &target])
                                .status();
                            let _ = std::process::Command::new("tmux")
                                .args(["attach-session", "-t", "agents"])
                                .status();
                            // Restore TUI when user detaches
                            enable_raw_mode()?;
                            io::stdout().execute(EnterAlternateScreen)?;
                            terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
                        }
                    }
                    KeyCode::Char('y') if approval_mode => app.approve_request(),
                    KeyCode::Char('n') if approval_mode => app.deny_request(),
                    KeyCode::Tab => app.toggle_focus(),
                    KeyCode::Up => app.move_selection_up(),
                    KeyCode::Down => app.move_selection_down(),
                    KeyCode::Enter => app.submit_answer(),
                    KeyCode::Backspace => {
                        app.input_text.pop();
                    }
                    KeyCode::Char(c) => {
                        app.input_text.push(c);
                    }
                    KeyCode::Esc => {
                        app.input_text.clear();
                    }
                    _ => {}
                }
            }
        }

        while let Ok(event) = event_rx.try_recv() {
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
