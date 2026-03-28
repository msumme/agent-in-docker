mod app;
mod ui;

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use app::App;
use orchestrator_core::types::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up logging to file (can't log to terminal since TUI owns it)
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

    // Start the WebSocket server
    let server_addr = addr.clone();
    tokio::spawn(async move {
        if let Err(e) = orchestrator_core::server::run(&server_addr, event_tx, cmd_rx).await {
            tracing::error!("Server error: {}", e);
        }
    });

    // Set up terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cmd_tx.clone());

    // Main loop
    loop {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        // Poll for crossterm events with a short timeout so we can also check orchestrator events
        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') if app.pending_requests.is_empty() => {
                        app.should_quit = true;
                    }
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

        // Drain orchestrator events
        while let Ok(event) = event_rx.try_recv() {
            app.handle_event(event);
        }

        if app.should_quit {
            let _ = cmd_tx.send(TuiCommand::Shutdown);
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
