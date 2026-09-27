use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::state::{Message, Role, UiState};

/// Draw the whole screen: transcript, permission overlay, input, status.
pub fn render(frame: &mut Frame, state: &UiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    draw_transcript(frame, state, chunks[0]);
    draw_input(frame, state, chunks[1]);
    draw_status(frame, state, chunks[2]);

    if state.permission.is_some() {
        draw_permission(frame, state);
    }
}

fn draw_transcript(frame: &mut Frame, state: &UiState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for m in transcript_lines(state) {
        push_message(&mut lines, &m);
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " (no messages yet) ",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Auto-follow: keep the bottom pinned while new output streams in.
    let visible = area.height.saturating_sub(2) as usize;
    let scroll = if lines.len() > visible && state.follow_bottom {
        lines.len() - visible
    } else {
        0
    };

    let widget = Paragraph::new(lines)
        .block(Block::default().title(" ccb "))
        .wrap(Wrap { trim: true })
        .scroll((scroll as u16, 0));
    frame.render_widget(widget, area);
}

/// The committed messages plus the live streaming tail, in render order.
fn transcript_lines(state: &UiState) -> Vec<Message> {
    let mut out: Vec<Message> = state.messages.clone();
    if let Some(streaming) = &state.streaming {
        if !streaming.is_empty() {
            out.push(Message::new(Role::Assistant, streaming.clone()));
        }
    }
    if let Some(thinking) = &state.thinking {
        if !thinking.is_empty() {
            out.push(Message::new(Role::Thought, thinking.clone()));
        }
    }
    for tool in &state.tools {
        let label = tool.title.clone().unwrap_or(tool.name.clone());
        let body = if tool.output.is_empty() {
            format!("{label} [{}]", tool.status.as_deref().unwrap_or(""))
        } else {
            format!(
                "{label} [{}]\n{}",
                tool.status.as_deref().unwrap_or(""),
                tool.output
            )
        };
        out.push(Message::new(Role::Tool, body));
    }
    out
}

fn push_message(lines: &mut Vec<Line>, m: &Message) {
    let (label, style) = match m.role {
        Role::User => ("you › ", Style::default().fg(Color::Cyan)),
        Role::Assistant => ("assistant › ", Style::default().fg(Color::Gray)),
        Role::Thought => ("thinking › ", Style::default().fg(Color::DarkGray)),
        Role::Tool => ("tool › ", Style::default().fg(Color::Yellow)),
    };
    let bold = style.add_modifier(Modifier::BOLD);
    lines.push(Line::from(vec![Span::styled(label, bold)]));
    for paragraph in m.text.split('\n') {
        lines.push(Line::from(vec![Span::styled(format!("    {paragraph}"), style)]));
    }
    lines.push(Line::from(""));
}

fn draw_input(frame: &mut Frame, state: &UiState, area: Rect) {
    let shown = if state.input.is_empty() {
        Span::styled(
            "(type a prompt, enter to send · up/down = history)",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        Span::raw(state.input.clone())
    };
    let widget = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Yellow)),
        shown,
    ]))
    .block(Block::default().border_style(Style::default().fg(Color::DarkGray)));
    frame.render_widget(widget, area);
}

fn draw_status(frame: &mut Frame, state: &UiState, area: Rect) {
    let session = state
        .session_id
        .as_deref()
        .map(|s| s.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "connecting".to_string());

    let mut spans: Vec<Span> = vec![Span::styled(
        format!("{} · session {} ", if state.busy { "busy" } else { "idle" }, session),
        Style::default().fg(Color::DarkGray),
    )];
    if let Some((used, size, cost)) = state.usage {
        spans.push(Span::styled(
            format!("· {} / {} tokens ", used, size),
            Style::default().fg(Color::Blue),
        ));
        if let Some(usd) = cost {
            spans.push(Span::styled(
                format!("· ${usd:.4} "),
                Style::default().fg(Color::Blue),
            ));
        }
    }
    if !state.status.is_empty() {
        spans.push(Span::styled(format!("· {} ", state.status), Style::default().fg(Color::Red)));
    }
    spans.push(Span::styled(
        "· enter=send · q=quit",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Centered approval dialog over the transcript while the turn is blocked.
fn draw_permission(frame: &mut Frame, state: &UiState) {
    let perm = match &state.permission {
        Some(p) => p,
        None => return,
    };
    let area = frame.area();
    let height = (perm.options.len() as u16 + 6).min(area.height.saturating_sub(4));
    let width = area.width.saturating_sub(8).max(40);
    let popup = centered(width, height, area);

    frame.render_widget(Clear, popup);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        format!(" {} ", perm.title),
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )]));
    if let Some(tool) = &perm.tool_name {
        lines.push(Line::from(vec![Span::styled(
            format!(" tool: {tool} "),
            Style::default().fg(Color::Gray),
        )]));
    }
    if let Some(desc) = &perm.description {
        lines.push(Line::from(vec![Span::styled(
            format!(" {desc} "),
            Style::default().fg(Color::Gray),
        )]));
    }
    lines.push(Line::from(""));
    for (idx, opt) in perm.options.iter().enumerate() {
        let marker = if idx == perm.selected { " ▸ " } else { "   " };
        let style = if idx == perm.selected {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![Span::styled(
            format!("{marker}{}  [{}]", opt.name, opt.kind),
            style,
        )]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " ↑/↓ or 1-9 to choose · enter to confirm · esc to reject ",
        Style::default().fg(Color::DarkGray),
    )]));

    let widget = Paragraph::new(lines)
        .block(Block::default().borders(ratatui::widgets::Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, popup);
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
