use crate::protocol::{AcpEvent, PermissionOption, RequestId};

/// One rendered line of the conversation.
#[derive(Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// The assistant's private reasoning, rendered dim.
    Thought,
    /// A tool call and its output.
    Tool,
}

impl Message {
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self { role, text: text.into() }
    }
}

/// A tool call in flight or finished, accumulated by `toolCallId`.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub name: String,
    pub title: Option<String>,
    pub status: Option<String>,
    pub output: String,
}

/// Pending permission prompt the user must answer before the turn continues.
#[derive(Clone, Debug)]
pub struct PendingPermission {
    pub id: RequestId,
    pub title: String,
    pub description: Option<String>,
    pub tool_name: Option<String>,
    pub options: Vec<PermissionOption>,
    /// Highlighted option index for arrow/number key selection.
    pub selected: usize,
}

/// Everything the TUI renders and the input buffer state.
#[derive(Default)]
pub struct UiState {
    pub messages: Vec<Message>,
    pub input: String,
    /// Up/down arrow history; `history_pos` is an index from the end.
    pub history: Vec<String>,
    pub history_pos: usize,
    pub session_id: Option<String>,
    pub busy: bool,
    /// Accumulated assistant text for the message currently streaming.
    pub streaming: Option<String>,
    /// Accumulated thinking for the current turn.
    pub thinking: Option<String>,
    pub tools: Vec<ToolCall>,
    pub permission: Option<PendingPermission>,
    pub usage: Option<(u64, u64, Option<f64>)>,
    pub status: String,
    pub quit: bool,
    /// Did the input scroll past the end? Pin the view to the bottom then.
    pub follow_bottom: bool,
}

impl UiState {
    /// Apply one protocol event to the view state.
    pub fn apply(&mut self, event: AcpEvent) {
        match event {
            AcpEvent::SessionReady { session_id } => {
                self.session_id = Some(session_id);
            }
            AcpEvent::AgentMessageChunk { text, .. } => {
                self.streaming.get_or_insert_with(String::new).push_str(&text);
                self.follow_bottom = true;
            }
            AcpEvent::AgentMessage { text, .. } => {
                // Final message supersedes the chunks.
                self.streaming = Some(text);
                self.finish_turn();
            }
            AcpEvent::AgentThoughtChunk { text, .. } => {
                self.thinking.get_or_insert_with(String::new).push_str(&text);
            }
            AcpEvent::AgentThought { text, .. } => {
                self.thinking = Some(text);
            }
            AcpEvent::ToolCallUpdate {
                name,
                title,
                status,
                content_text,
            } => {
                // ACP updates are keyed by toolCallId; the client keeps one
                // running tool slot per turn, which is what the UI needs.
                if let Some(tool) = self.tools.last_mut() {
                    if let Some(n) = name {
                        tool.name = n;
                    }
                    if let Some(t) = title {
                        tool.title = Some(t);
                    }
                    if let Some(s) = status {
                        tool.status = Some(s);
                    }
                    if let Some(c) = content_text {
                        tool.output = c;
                    }
                } else {
                    self.tools.push(ToolCall {
                        name: name.unwrap_or_default(),
                        title,
                        status,
                        output: content_text.unwrap_or_default(),
                    });
                }
                self.follow_bottom = true;
            }
            AcpEvent::ToolCallContentChunk { text, .. } => {
                if let Some(tool) = self.tools.last_mut() {
                    tool.output.push_str(&text);
                }
                self.follow_bottom = true;
            }
            AcpEvent::UsageUpdate { used, size, cost_usd } => {
                self.usage = Some((used, size, cost_usd));
            }
            AcpEvent::RequestPermission(req) => {
                let options = req.options;
                // Default to the first allow option if there is one.
                let selected = options
                    .iter()
                    .position(|o| o.kind.starts_with("allow"))
                    .unwrap_or(0);
                self.permission = Some(PendingPermission {
                    id: req.id,
                    title: req.title,
                    description: req.description,
                    tool_name: req.tool_name,
                    options,
                    selected,
                });
            }
            AcpEvent::Error { message } => {
                self.status = message;
            }
            AcpEvent::Closed => {
                self.status = "connection closed".to_string();
                self.busy = false;
            }
        }
    }

    /// A turn finished: commit streaming + thinking + tools into the transcript.
    pub fn finish_turn(&mut self) {
        if let Some(text) = self.streaming.take().filter(|t| !t.is_empty()) {
            self.messages.push(Message::new(Role::Assistant, text));
        }
        if let Some(thought) = self.thinking.take().filter(|t| !t.is_empty()) {
            self.messages.push(Message::new(Role::Thought, thought));
        }
        for tool in std::mem::take(&mut self.tools) {
            let label = tool.title.unwrap_or(tool.name);
            let body = if tool.output.is_empty() {
                format!("{label} [{}]", tool.status.unwrap_or_default())
            } else {
                format!("{label} [{}]\n{}", tool.status.unwrap_or_default(), tool.output)
            };
            self.messages.push(Message::new(Role::Tool, body));
        }
        self.busy = false;
    }

    /// Record a submitted user prompt and open the streaming slot.
    pub fn submit(&mut self, prompt: String) {
        self.messages.push(Message::new(Role::User, prompt.clone()));
        self.history.push(prompt);
        self.history_pos = 0;
        self.streaming = Some(String::new());
        self.thinking = None;
        self.tools.clear();
        self.permission = None;
        self.busy = true;
        self.follow_bottom = true;
    }

    /// Move up (older) in the history buffer.
    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_pos < self.history.len() {
            self.history_pos += 1;
            let idx = self.history.len() - self.history_pos;
            self.input = self.history[idx].clone();
        }
    }

    /// Move down (newer) in the history buffer.
    pub fn history_down(&mut self) {
        if self.history_pos == 0 {
            return;
        }
        self.history_pos -= 1;
        self.input = if self.history_pos == 0 {
            String::new()
        } else {
            let idx = self.history.len() - self.history_pos;
            self.history[idx].clone()
        };
    }

}
