use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, FocusPanel};

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),    // top area (agents + output)
            Constraint::Length(12), // pending requests
            Constraint::Length(1), // status bar
        ])
        .split(frame.area());

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(40)])
        .split(chunks[0]);

    draw_agents_panel(frame, app, top_chunks[0]);
    draw_log_panel(frame, app, top_chunks[1]);
    draw_requests_panel(frame, app, chunks[1]);
    draw_status_bar(frame, app, chunks[2]);
}

fn draw_agents_panel(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.focus == FocusPanel::Agents {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = app
        .agents
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let indicator = "●";
            let style = if i == app.selected_agent && app.focus == FocusPanel::Agents {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green)
            };
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!(" {} {}", indicator, agent.name),
                    style,
                )),
                Line::from(Span::styled(
                    format!("   role: {}", agent.role),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();

    let block = Block::default()
        .title(" Agents ")
        .borders(Borders::ALL)
        .border_style(border_style);

    if items.is_empty() {
        let empty = Paragraph::new("  No agents connected")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
    } else {
        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }
}

fn draw_log_panel(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .completed_log
        .iter()
        .rev()
        .take(area.height as usize - 2)
        .rev()
        .map(|entry| ListItem::new(Line::from(Span::raw(format!(" {}", entry)))))
        .collect();

    let block = Block::default()
        .title(" Activity Log ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_requests_panel(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.focus == FocusPanel::Requests {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Pending Requests ")
        .borders(Borders::ALL)
        .border_style(border_style);

    if app.pending_requests.is_empty() {
        let empty = Paragraph::new("  Waiting for agent requests...")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner area: request list + input
    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(inner);

    // Request list
    let items: Vec<ListItem> = app
        .pending_requests
        .iter()
        .enumerate()
        .map(|(i, req)| {
            let selected = i == app.selected_request && app.focus == FocusPanel::Requests;
            let prefix = if selected { ">" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!(
                    "{} [{}] {}: {}",
                    prefix, req.agent_name, req.request_type, req.question
                ),
                style,
            )))
        })
        .collect();

    let req_list = List::new(items);
    frame.render_widget(req_list, inner_chunks[0]);

    // Input area
    let input_block = Block::default()
        .title(" Answer (Enter to submit) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.focus == FocusPanel::Requests {
            Color::Yellow
        } else {
            Color::DarkGray
        }));

    let input = Paragraph::new(format!(" {}", app.input_text))
        .block(input_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(input, inner_chunks[1]);

    // Show cursor in input field
    if app.focus == FocusPanel::Requests {
        frame.set_cursor_position((
            inner_chunks[1].x + 2 + app.input_text.len() as u16,
            inner_chunks[1].y + 1,
        ));
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status = format!(
        " Agents: {} | Pending: {} | Tab: switch | a: attach | Enter: submit | q: quit",
        app.agents.len(),
        app.pending_requests.len()
    );
    let bar = Paragraph::new(status).style(
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray),
    );
    frame.render_widget(bar, area);
}
