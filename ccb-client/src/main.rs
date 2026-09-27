mod daemon;
mod protocol;
mod state;
mod ui;

use std::error::Error;
use std::time::Duration;

use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal;
use ratatui::DefaultTerminal;
use ratatui::backend::CrosstermBackend;

use daemon::AcpClient;
use protocol::default_socket_path;
use state::UiState;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let socket = args
        .get(1)
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_socket_path);

    let client = AcpClient::connect(&socket)?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();

    let mut state = UiState { status: "connecting".into(), ..Default::default() };
    client.initialize()?;
    match client.new_session(&cwd) {
        Ok(session_id) => {
            state.session_id = Some(session_id);
            state.status = String::new();
        }
        Err(e) => {
            state.status = format!("session/new failed: {e}");
        }
    }

    terminal::enable_raw_mode()?;
    let mut term = DefaultTerminal::new(CrosstermBackend::new(std::io::stdout()))?;
    execute!(term.backend_mut(), terminal::EnterAlternateScreen)?;

    while !state.quit {
        term.draw(|f| ui::render(f, &state))?;
        drain_events(&client, &mut state);

        if event::poll(Duration::from_millis(50))? {
            if let CEvent::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                handle_key(&client, &mut state, key.code, key.modifiers);
            }
        }
    }

    execute!(term.backend_mut(), terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

/// Handle one key, routing to the permission dialog when it is open.
fn handle_key(client: &AcpClient, state: &mut UiState, code: KeyCode, mods: KeyModifiers) {
    if state.permission.is_some() {
        handle_permission_key(client, state, code);
        return;
    }
    match code {
        KeyCode::Enter => submit(client, state),
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
            let _ = client.cancel(state.session_id.as_deref().unwrap_or(""));
            state.status = "cancelling…".into();
        }
        KeyCode::Up => state.history_up(),
        KeyCode::Down => state.history_down(),
        KeyCode::Char('q') => state.quit = true,
        KeyCode::Char(c) => {
            state.input.push(c);
            state.follow_bottom = true;
        }
        KeyCode::Backspace => {
            state.input.pop();
        }
        _ => {}
    }
}

/// Keys while the permission dialog holds the turn hostage.
fn handle_permission_key(client: &AcpClient, state: &mut UiState, code: KeyCode) {
    let perm = match state.permission.as_mut() {
        Some(p) => p,
        None => return,
    };
    let id = perm.id;
    let count = perm.options.len();
    match code {
        KeyCode::Up => {
            if perm.selected > 0 {
                perm.selected -= 1;
            }
        }
        KeyCode::Down => {
            if perm.selected + 1 < count {
                perm.selected += 1;
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let idx = c.to_digit(10).unwrap_or(0) as usize;
            if idx >= 1 && idx <= count {
                perm.selected = idx - 1;
            }
        }
        KeyCode::Enter => {
            let choice = state
                .permission
                .as_ref()
                .and_then(|p| p.options.get(p.selected))
                .cloned();
            let _ = client.respond_permission(id, choice.as_ref());
            state.permission = None;
        }
        KeyCode::Esc => {
            let _ = client.respond_permission(id, None);
            state.permission = None;
            state.status = "permission rejected".into();
        }
        _ => {}
    }
}

/// Send the current input buffer as a prompt.
fn submit(client: &AcpClient, state: &mut UiState) {
    let text = std::mem::take(&mut state.input);
    if text.is_empty() || state.busy {
        return;
    }
    let session_id = match state.session_id.clone() {
        Some(s) => s,
        None => {
            state.status = "no session".into();
            return;
        }
    };
    if client.prompt(&session_id, &text).is_err() {
        state.status = "send failed".into();
        return;
    }
    state.submit(text);
    state.status = String::new();
}

/// Consume every event the daemon has already produced.
fn drain_events(client: &AcpClient, state: &mut UiState) {
    loop {
        let event = client.try_recv();
        match event {
            Some(ev) => {
                let closed = matches!(ev, protocol::AcpEvent::Closed);
                state.apply(ev);
                if closed {
                    return;
                }
            }
            None => return,
        }
    }
}
