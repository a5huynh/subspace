use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use ratatui::widgets::*;
use tokio::sync::mpsc;

use subspace::agent::{Agent, AgentEvent};

// ---------------------------------------------------------------------------
// Display messages — what we render in the chat pane
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum ChatEntry {
    User(String),
    AssistantText(String),
    Thinking(String),
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        error: bool,
        summary: String,
    },
    Status(String),
    Error(String),
}

// ---------------------------------------------------------------------------
// TUI state
// ---------------------------------------------------------------------------

struct TuiState {
    input: String,
    cursor_pos: usize,
    chat: Vec<ChatEntry>,
    scroll_offset: u16,
    is_streaming: bool,
    status: String,
    should_quit: bool,
    /// When streaming, accumulates the current assistant text
    current_assistant_text: String,
}

impl TuiState {
    fn new(model: &str) -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
            chat: Vec::new(),
            scroll_offset: 0,
            is_streaming: false,
            status: format!("Model: {model} | Ctrl+C: quit | Esc: abort"),
            should_quit: false,
            current_assistant_text: String::new(),
        }
    }

    fn push_chat(&mut self, entry: ChatEntry) {
        self.chat.push(entry);
        // Auto-scroll to bottom
        self.scroll_offset = u16::MAX;
    }

    fn flush_assistant_text(&mut self) {
        if !self.current_assistant_text.is_empty() {
            let text = std::mem::take(&mut self.current_assistant_text);
            self.push_chat(ChatEntry::AssistantText(text));
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub async fn run(agent: Arc<Agent>) -> anyhow::Result<()> {
    // Setup terminal
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let model = agent.model().to_string();
    let mut state = TuiState::new(&model);

    // Channel for agent events during streaming
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(512);

    // Track active agent task
    let mut agent_handle: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        // Draw
        terminal.draw(|f| draw(f, &mut state))?;

        // Poll for terminal events (non-blocking) and agent events
        tokio::select! {
            // Terminal input (poll with short timeout)
            _ = tokio::time::sleep(Duration::from_millis(16)) => {
                while event::poll(Duration::ZERO)? {
                    if let Event::Key(key) = event::read()? {
                        handle_key(key, &mut state, &agent, &agent_tx, &mut agent_handle).await;
                    }
                }
            }
            // Agent events
            Some(event) = agent_rx.recv() => {
                handle_agent_event(event, &mut state);
            }
        }

        if state.should_quit {
            break;
        }
    }

    // Cleanup terminal
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    // Cancel any running agent task
    if let Some(handle) = agent_handle {
        handle.abort();
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

async fn handle_key(
    key: event::KeyEvent,
    state: &mut TuiState,
    agent: &Arc<Agent>,
    agent_tx: &mpsc::Sender<AgentEvent>,
    agent_handle: &mut Option<tokio::task::JoinHandle<()>>,
) {
    match (key.modifiers, key.code) {
        // Ctrl+C — quit (double press) or clear input
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if state.input.is_empty() && !state.is_streaming {
                state.should_quit = true;
            } else if state.is_streaming {
                // Abort agent
                agent.abort().await;
                state.is_streaming = false;
                state.flush_assistant_text();
                state.push_chat(ChatEntry::Status("Aborted".into()));
            } else {
                state.input.clear();
                state.cursor_pos = 0;
            }
        }
        // Escape — abort if streaming
        (_, KeyCode::Esc) => {
            if state.is_streaming {
                agent.abort().await;
                state.is_streaming = false;
                state.flush_assistant_text();
                state.push_chat(ChatEntry::Status("Aborted".into()));
            }
        }
        // Enter — submit input
        (_, KeyCode::Enter) if !state.is_streaming => {
            let text = state.input.trim().to_string();
            if text.is_empty() {
                return;
            }
            state.input.clear();
            state.cursor_pos = 0;
            state.push_chat(ChatEntry::User(text.clone()));
            state.is_streaming = true;
            state.current_assistant_text.clear();

            // Spawn agent prompt
            let agent = agent.clone();
            let tx = agent_tx.clone();

            let handle = tokio::spawn(async move {
                let (mut rx, task) = agent.prompt(&text).await;
                while let Some(event) = rx.recv().await {
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
                let _ = task.await;
            });

            *agent_handle = Some(handle);
        }
        // Backspace
        (_, KeyCode::Backspace) if !state.is_streaming => {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
                state.input.remove(state.cursor_pos);
            }
        }
        // Delete
        (_, KeyCode::Delete) if !state.is_streaming => {
            if state.cursor_pos < state.input.len() {
                state.input.remove(state.cursor_pos);
            }
        }
        // Arrow keys
        (_, KeyCode::Left) if !state.is_streaming => {
            state.cursor_pos = state.cursor_pos.saturating_sub(1);
        }
        (_, KeyCode::Right) if !state.is_streaming => {
            state.cursor_pos = (state.cursor_pos + 1).min(state.input.len());
        }
        (_, KeyCode::Home) if !state.is_streaming => {
            state.cursor_pos = 0;
        }
        (_, KeyCode::End) if !state.is_streaming => {
            state.cursor_pos = state.input.len();
        }
        // Scroll chat
        (_, KeyCode::PageUp) => {
            state.scroll_offset = state.scroll_offset.saturating_sub(10);
        }
        (_, KeyCode::PageDown) => {
            state.scroll_offset = state.scroll_offset.saturating_add(10);
        }
        // Ctrl+U — clear line
        (KeyModifiers::CONTROL, KeyCode::Char('u')) if !state.is_streaming => {
            state.input.clear();
            state.cursor_pos = 0;
        }
        // Regular character input
        (_, KeyCode::Char(c)) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, c);
            state.cursor_pos += 1;
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Agent event handling
// ---------------------------------------------------------------------------

fn handle_agent_event(event: AgentEvent, state: &mut TuiState) {
    match event {
        AgentEvent::TextDelta(text) => {
            state.current_assistant_text.push_str(&text);
        }
        AgentEvent::ThinkingDelta(text) => {
            // Accumulate thinking into a separate entry if we want to show it
            // For now, just show a brief indicator
            if let Some(ChatEntry::Thinking(t)) = state.chat.last_mut() {
                t.push_str(&text);
            } else {
                state.flush_assistant_text();
                state.push_chat(ChatEntry::Thinking(text));
            }
        }
        AgentEvent::ToolCallStart {
            name, arguments, ..
        } => {
            state.flush_assistant_text();
            let args = arguments.to_string();
            let args_short = if args.len() > 100 {
                format!("{}...", &args[..100])
            } else {
                args
            };
            state.push_chat(ChatEntry::ToolCall {
                name,
                args: args_short,
            });
        }
        AgentEvent::ToolExecEnd { name, result, .. } => {
            let summary = result
                .content
                .first()
                .and_then(|c| c.as_text())
                .unwrap_or("")
                .to_string();
            let summary = if summary.len() > 200 {
                format!("{}...", &summary[..200])
            } else {
                summary
            };
            state.push_chat(ChatEntry::ToolResult {
                name,
                error: result.is_error,
                summary,
            });
        }
        AgentEvent::TurnEnd { turn, usage, .. } => {
            state.flush_assistant_text();
            state.push_chat(ChatEntry::Status(format!(
                "turn {turn} | tokens: {}",
                usage.total()
            )));
        }
        AgentEvent::AgentEnd { total_usage, .. } => {
            state.flush_assistant_text();
            state.is_streaming = false;
            state.push_chat(ChatEntry::Status(format!(
                "done | total tokens: {}",
                total_usage.total()
            )));
        }
        AgentEvent::Warning(w) => {
            state.flush_assistant_text();
            state.push_chat(ChatEntry::Status(format!("⚠ {w}")));
        }
        AgentEvent::Error(e) => {
            state.flush_assistant_text();
            state.is_streaming = false;
            state.push_chat(ChatEntry::Error(e));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(f: &mut Frame, state: &mut TuiState) {
    let area = f.area();

    // Layout: chat | input (3 lines) | footer (1 line)
    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);

    draw_chat(f, chunks[0], state);
    draw_input(f, chunks[1], state);
    draw_footer(f, chunks[2], state);
}

fn draw_chat(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    for entry in &state.chat {
        match entry {
            ChatEntry::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("You: ", Style::default().fg(Color::Green).bold()),
                    Span::raw(text),
                ]));
            }
            ChatEntry::AssistantText(text) => {
                // Wrap long assistant text into multiple lines
                for (i, line) in text.lines().enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![
                            Span::styled("AI: ", Style::default().fg(Color::Cyan).bold()),
                            Span::raw(line),
                        ]));
                    } else {
                        lines.push(Line::from(vec![Span::raw("    "), Span::raw(line)]));
                    }
                }
            }
            ChatEntry::Thinking(text) => {
                let preview = if text.len() > 80 {
                    format!("{}...", &text[..80])
                } else {
                    text.clone()
                };
                lines.push(Line::from(Span::styled(
                    format!("  💭 {preview}"),
                    Style::default().fg(Color::DarkGray).italic(),
                )));
            }
            ChatEntry::ToolCall { name, args } => {
                lines.push(Line::from(vec![
                    Span::styled("  ▶ ", Style::default().fg(Color::Yellow)),
                    Span::styled(name, Style::default().fg(Color::Yellow).bold()),
                    Span::styled(format!("({args})"), Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatEntry::ToolResult {
                name,
                error,
                summary,
            } => {
                let (icon, color) = if *error {
                    ("  ✗ ", Color::Red)
                } else {
                    ("  ✓ ", Color::Green)
                };
                lines.push(Line::from(vec![
                    Span::styled(icon, Style::default().fg(color)),
                    Span::styled(format!("{name}: "), Style::default().fg(color)),
                    Span::styled(summary, Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatEntry::Status(text) => {
                lines.push(Line::from(Span::styled(
                    format!("  --- {text} ---"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            ChatEntry::Error(text) => {
                lines.push(Line::from(Span::styled(
                    format!("  ✗ Error: {text}"),
                    Style::default().fg(Color::Red).bold(),
                )));
            }
        }
        // Blank line between entries
        lines.push(Line::from(""));
    }

    // If currently streaming assistant text, show it as in-progress
    if !state.current_assistant_text.is_empty() {
        for (i, line) in state.current_assistant_text.lines().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled("AI: ", Style::default().fg(Color::Cyan).bold()),
                    Span::raw(line),
                ]));
            } else {
                lines.push(Line::from(vec![Span::raw("    "), Span::raw(line)]));
            }
        }
        if state.is_streaming {
            lines.push(Line::from(Span::styled(
                "    ▌",
                Style::default().fg(Color::Cyan),
            )));
        }
    }

    let total_lines = lines.len() as u16;
    let visible_height = area.height.saturating_sub(2); // account for borders

    // Clamp scroll offset
    let max_scroll = total_lines.saturating_sub(visible_height);
    if state.scroll_offset > max_scroll {
        state.scroll_offset = max_scroll;
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" subspace ")
                .title_alignment(Alignment::Center),
        )
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));

    f.render_widget(paragraph, area);
}

fn draw_input(f: &mut Frame, area: Rect, state: &TuiState) {
    let border_color = if state.is_streaming {
        Color::Yellow
    } else {
        Color::Blue
    };

    let title = if state.is_streaming {
        " streaming... (Esc to abort) "
    } else {
        " message (Enter to send) "
    };

    let input = Paragraph::new(state.input.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(title),
    );

    f.render_widget(input, area);

    // Show cursor in input area
    if !state.is_streaming {
        f.set_cursor_position(Position::new(
            area.x + 1 + state.cursor_pos as u16,
            area.y + 1,
        ));
    }
}

fn draw_footer(f: &mut Frame, area: Rect, state: &TuiState) {
    let footer = Paragraph::new(state.status.as_str()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(footer, area);
}
