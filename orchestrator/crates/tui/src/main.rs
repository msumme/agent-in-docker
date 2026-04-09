mod app;
mod ui;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
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

    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "0.0.0.0:9800".to_string());
    // Optional second arg: project root (defaults to current dir)
    let project_root = args.next()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let project_config = Arc::new(orchestrator_core::project_config::ProjectConfig::from_root(project_root));

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<OrchestratorEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();

    let permissions: Box<dyn orchestrator_core::mcp::PermissionCheck> = {
        let mut checker = orchestrator_core::permissions::PermissionChecker::new(
            Box::new(orchestrator_core::permissions::RealEnvResolver),
        );
        let roles_dir = project_config.project_root.join("roles");
        let _ = checker.load_roles_from_dir(&roles_dir);
        Box::new(checker)
    };
    let mcp_state = Arc::new(McpState::new(event_tx.clone(), permissions));

    let agent_mgr = Arc::new(std::sync::Mutex::new(
        orchestrator_core::agent_manager::AgentManager::new(
            "orchestrator".into(),
            Box::new(orchestrator_core::agent_manager::RealTmuxOps),
            Box::new(orchestrator_core::agent_manager::RealContainerOps),
            Box::new(orchestrator_core::agent_manager::RealShellOps),
        ),
    ));

    let server_addr = addr.clone();
    let mcp_for_server = mcp_state.clone();
    let mgr_for_server = agent_mgr.clone();
    let cfg_for_server = project_config.clone();
    tokio::spawn(async move {
        if let Err(e) = orchestrator_core::server::run(&server_addr, event_tx, cmd_rx, Some(mcp_for_server), Some(mgr_for_server), Some(cfg_for_server)).await {
            tracing::error!("Server error: {}", e);
        }
    });

    let mcp_app = mcp_router(mcp_state.clone());
    let ws_port: u16 = addr.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(9800);
    let http_addr = format!("0.0.0.0:{}", ws_port + 1);
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(&http_addr).await.unwrap();
        tracing::info!("MCP HTTP server listening on {}", http_addr);
        axum::serve(listener, mcp_app).await.unwrap();
    });

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cmd_tx.clone());

    loop {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match app.handle_key(key.code) {
                    app::KeyEffect::Quit => {
                        let _ = cmd_tx.send(TuiCommand::Shutdown);
                        break;
                    }
                    app::KeyEffect::AttachAgent(name) => {
                        let mgr = agent_mgr.lock().unwrap();
                        let _ = mgr.attach_to_agent(&name);
                    }
                    app::KeyEffect::None => {}
                }
            }
        }

        while let Ok(event) = event_rx.try_recv() {
            app.handle_event(event);
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
