use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver};
use std::sync::atomic::{AtomicU64, Ordering};
use parking_lot::Mutex;

use crate::protocol::{
    parse_permission_request, parse_session_update, AgentMessage, AcpEvent, ClientMessage,
    PermissionOption, RequestId,
};

/// Append a line to $CCB_DEBUG when set, so the wire is inspectable without a
/// debugger. ponytail: file log only; remove by unsetting the env var.
fn dbg(line: &str) {
    use std::io::Write;
    if let Ok(path) = std::env::var("CCB_DEBUG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&path) {
            let _ = writeln!(f, "{line}");
        }
    }
}
/// over newline-delimited JSON. A reader thread turns agent messages into
/// [AcpEvent]s on a channel; the main loop sends requests and answers the
/// agent's permission requests by id.
pub struct AcpClient {
    writer: Mutex<UnixStream>,
    next_id: AtomicU64,
    events: Receiver<AcpEvent>,
}

impl AcpClient {
    /// Connect to the daemon socket at `path`.
    pub fn connect(path: &str) -> std::io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        Self::open(stream)
    }

    /// Wrap an already-connected stream: spawn the reader, keep the writer.
    pub fn open(stream: UnixStream) -> std::io::Result<Self> {
        let reader = stream
            .try_clone()
            .map_err(|e| std::io::Error::other(format!("cannot clone socket: {e}")))?;
        let (tx, rx) = mpsc::channel::<AcpEvent>();
        std::thread::spawn(move || drain(reader, tx));
        Ok(Self {
            writer: Mutex::new(stream),
            next_id: AtomicU64::new(1),
            events: rx,
        })
    }

    fn send(&self, msg: &ClientMessage) -> std::io::Result<()> {
        let line = serde_json::to_string(msg).map_err(std::io::Error::other)?;
        dbg(&format!("> {line}"));
        let mut w = self.writer.lock();
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()?;
        Ok(())
    }

    /// Allocate the next request id and send a request. Returns the id so the
    /// caller can wait for the matching response.
    fn request(&self, method: &str, params: serde_json::Value) -> std::io::Result<RequestId> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.send(&ClientMessage::Request {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        })?;
        Ok(id)
    }

    fn notify(&self, method: &str, params: serde_json::Value) -> std::io::Result<()> {
        self.send(&ClientMessage::Notification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        })
    }

    /// `initialize`: capability handshake, sent as a real request (the SDK
    /// only starts initialization on a request with an id; a notification is
    /// rejected and blocks every later method). Waits for the response.
    pub fn initialize(&self) -> std::io::Result<()> {
        self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-07-01",
                "clientCapabilities": {},
            }),
        )?;
        match self.events.recv() {
            Ok(AcpEvent::Closed) | Err(_) => Err(std::io::Error::other(
                "connection closed during initialize",
            )),
            // Any event means the initialize round-trip completed.
            Ok(_) => Ok(()),
        }
    }

    /// `session/new`. Returns the session id. Blocks for the response.
    pub fn new_session(&self, cwd: &str) -> std::io::Result<String> {
        let _id = self.request(
            "session/new",
            serde_json::json!({ "cwd": cwd, "mcpServers": [] }),
        )?;
        // Wait for the response: the reader turns it into SessionReady.
        while let Ok(event) = self.events.recv() {
            match event {
                AcpEvent::SessionReady { session_id } => return Ok(session_id),
                AcpEvent::Error { message } => {
                    return Err(std::io::Error::other(format!("session/new failed: {message}")))
                }
                // Anything else this early is noise; keep waiting.
                _ => continue,
            }
        }
        Err(std::io::Error::other(
            "connection closed before session/new answered",
        ))
    }

    /// `session/prompt` with a single text block. Fire and forget: the reply
    /// arrives as a stream of session/update notifications.
    pub fn prompt(&self, session_id: &str, text: &str) -> std::io::Result<()> {
        self.request(
            "session/prompt",
            serde_json::json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": text }],
            }),
        )?;
        Ok(())
    }

    /// `session/cancel` for the running turn.
    pub fn cancel(&self, session_id: &str) -> std::io::Result<()> {
        self.notify("session/cancel", serde_json::json!({ "sessionId": session_id }))
    }

    /// Answer a permission request by picking an option id, or cancelling.
    pub fn respond_permission(
        &self,
        id: RequestId,
        option: Option<&PermissionOption>,
    ) -> std::io::Result<()> {
        let result = match option {
            Some(o) => serde_json::json!({
                "outcome": { "outcome": "selected", "optionId": o.option_id }
            }),
            None => serde_json::json!({ "outcome": { "outcome": "cancelled" } }),
        };
        self.send(&ClientMessage::Response {
            jsonrpc: "2.0",
            id,
            result,
        })
    }

    /// Non-blocking event poll for the UI loop.
    pub fn try_recv(&self) -> Option<AcpEvent> {
        self.events.try_recv().ok()
    }

}

/// Read newline-delimited JSON until the socket closes, dispatching each
/// agent message to the right high level event.
fn drain(stream: UnixStream, tx: mpsc::Sender<AcpEvent>) {
    for item in BufReader::new(stream).lines() {
        let line = match item {
            Ok(line) => line,
            Err(_) => break,
        };
        dbg(&format!("< {line}"));
        if line.is_empty() {
            continue;
        }
        let msg: AgentMessage = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(_) => continue,
        };
        let event = dispatch(&msg);
        let is_terminal = matches!(event, AcpEvent::Closed);
        if tx.send(event).is_err() {
            return;
        }
        if is_terminal {
            return;
        }
    }
    let _ = tx.send(AcpEvent::Closed);
}

/// One agent message to one high level event.
fn dispatch(msg: &AgentMessage) -> AcpEvent {
    let method = msg.method.as_deref().unwrap_or("");
    match (method, msg.id) {
        // session/update notification.
        ("session/update", _) => {
            if let Some(params) = &msg.params {
                parse_session_update(params)
                    .unwrap_or_else(|| AcpEvent::Error {
                        message: "unhandled session/update variant".to_string(),
                    })
            } else {
                AcpEvent::Error {
                    message: "session/update with no params".to_string(),
                }
            }
        }
        // The agent asks us for permission; we must answer by id.
        ("session/request_permission", Some(id)) => {
            if let Some(params) = &msg.params {
                parse_permission_request(id, params)
            } else {
                AcpEvent::Error {
                    message: "request_permission with no params".to_string(),
                }
            }
        }
        // A response to one of our requests (session/new etc.).
        (_, Some(id)) => {
            if let Some(result) = &msg.result {
                if let Some(session_id) = result.get("sessionId").and_then(|v| v.as_str()) {
                    AcpEvent::SessionReady {
                        session_id: session_id.to_string(),
                    }
                } else if id > 0 {
                    // Some other response we don't model yet; ignore it.
                    AcpEvent::Error {
                        message: format!("unmodeled response to request {id}"),
                    }
                } else {
                    AcpEvent::Error {
                        message: "response with no session id".to_string(),
                    }
                }
            } else if let Some(err) = &msg.error {
                AcpEvent::Error {
                    message: format!("rpc error: {err}"),
                }
            } else {
                AcpEvent::Error {
                    message: "response with neither result nor error".to_string(),
                }
            }
        }
        ("", _) => AcpEvent::Closed,
        _ => AcpEvent::Error {
            message: format!("unhandled method {method:?}"),
        },
    }
}
