use crate::authorship::internal_db::{InternalDatabase, PromptDbRecord};
use crate::error::GitAiError;
use crate::git::repository::Repository;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use std::io;

/// Tab selection in the prompt picker
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    CurrentRepo,
}

/// State for the prompt picker TUI
struct PromptPickerState {
    /// All loaded prompts
    prompts: Vec<PromptDbRecord>,
    /// Currently selected index
    selected_index: usize,
    /// Current tab
    current_tab: Tab,
    /// Search query
    search_query: String,
    /// Whether search mode is active
    search_active: bool,
    /// Whether preview mode is active
    preview_mode: bool,
    /// Scroll offset for preview mode
    preview_scroll: usize,
    /// Whether more prompts can be loaded
    has_more: bool,
    /// Batch size for loading prompts
    batch_size: usize,
    /// Current working directory (for repo filtering)
    current_workdir: Option<String>,
    /// Title to display
    title: String,
}

impl PromptPickerState {
    fn new(repo: Option<Repository>, title: String) -> Result<Self, GitAiError> {
        let current_workdir = repo
            .as_ref()
            .and_then(|r| r.workdir().ok().map(|p| p.to_string_lossy().to_string()));

        let mut state = Self {
            prompts: Vec::new(),
            selected_index: 0,
            current_tab: if current_workdir.is_some() {
                Tab::CurrentRepo
            } else {
                Tab::All
            },
            search_query: String::new(),
            search_active: false,
            preview_mode: false,
            preview_scroll: 0,
            has_more: true,
            batch_size: 50,
            current_workdir,
            title,
        };

        // Load initial batch
        state.load_more_prompts()?;

        Ok(state)
    }

    fn load_more_prompts(&mut self) -> Result<(), GitAiError> {
        if !self.has_more {
            return Ok(());
        }

        let db = InternalDatabase::global()?;
        let db_guard = db
            .lock()
            .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

        let offset = self.prompts.len();
        let workdir_filter = match self.current_tab {
            Tab::All => None,
            Tab::CurrentRepo => self.current_workdir.as_deref(),
        };

        let new_prompts = if self.search_query.is_empty() {
            db_guard.list_prompts(workdir_filter, None, self.batch_size, offset)?
        } else {
            db_guard.search_prompts(&self.search_query, workdir_filter, self.batch_size, offset)?
        };

        if new_prompts.len() < self.batch_size {
            self.has_more = false;
        }

        self.prompts.extend(new_prompts);

        Ok(())
    }

    fn refresh_prompts(&mut self) -> Result<(), GitAiError> {
        // Reset state
        self.prompts.clear();
        self.selected_index = 0;
        self.has_more = true;

        // Load first batch
        self.load_more_prompts()?;

        Ok(())
    }

    fn next(&mut self) -> Result<(), GitAiError> {
        if self.prompts.is_empty() {
            return Ok(());
        }

        if self.selected_index < self.prompts.len() - 1 {
            self.selected_index += 1;

            // Load more if near bottom
            if self.selected_index >= self.prompts.len().saturating_sub(10) {
                self.load_more_prompts()?;
            }
        }

        Ok(())
    }

    fn previous(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    fn next_tab(&mut self) -> Result<(), GitAiError> {
        match self.current_tab {
            Tab::All => {
                if self.current_workdir.is_some() {
                    self.current_tab = Tab::CurrentRepo;
                    self.refresh_prompts()?;
                }
            }
            Tab::CurrentRepo => {
                self.current_tab = Tab::All;
                self.refresh_prompts()?;
            }
        }
        Ok(())
    }

    fn previous_tab(&mut self) -> Result<(), GitAiError> {
        match self.current_tab {
            Tab::All => {
                if self.current_workdir.is_some() {
                    self.current_tab = Tab::CurrentRepo;
                    self.refresh_prompts()?;
                }
            }
            Tab::CurrentRepo => {
                self.current_tab = Tab::All;
                self.refresh_prompts()?;
            }
        }
        Ok(())
    }

    fn get_selected(&self) -> Option<&PromptDbRecord> {
        self.prompts.get(self.selected_index)
    }
}

/// Reusable prompt picker with search, tabs, preview
/// Returns the selected PromptDbRecord or None if cancelled
pub fn pick_prompt(
    repo: Option<&Repository>,
    title: &str,
) -> Result<Option<PromptDbRecord>, GitAiError> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Initialize state
    let mut state = PromptPickerState::new(repo.cloned(), title.to_string())?;

    // Main event loop
    let result = loop {
        terminal.draw(|f| render(f, &state))?;

        if let Event::Key(key) = event::read()? {
            // Only handle key press events, not release
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match handle_key_event(&mut state, key)? {
                KeyResult::Continue => {}
                KeyResult::Exit => break None,
                KeyResult::Select => {
                    if let Some(prompt) = state.get_selected() {
                        break Some(prompt.clone());
                    }
                }
            }
        }
    };

    // Cleanup
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(result)
}

enum KeyResult {
    Continue,
    Exit,
    Select,
}

fn handle_key_event(state: &mut PromptPickerState, key: KeyEvent) -> Result<KeyResult, GitAiError> {
    if state.preview_mode {
        // Preview mode: Esc to close, Enter to select, arrows/jk to scroll
        match key.code {
            KeyCode::Esc => {
                state.preview_mode = false;
                state.preview_scroll = 0;
                Ok(KeyResult::Continue)
            }
            KeyCode::Enter => {
                state.preview_mode = false;
                state.preview_scroll = 0;
                Ok(KeyResult::Select)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.preview_scroll = state.preview_scroll.saturating_sub(1);
                Ok(KeyResult::Continue)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.preview_scroll = state.preview_scroll.saturating_add(1);
                Ok(KeyResult::Continue)
            }
            _ => Ok(KeyResult::Continue),
        }
    } else if state.search_active {
        // Search mode: type to search, Esc to clear
        match key.code {
            KeyCode::Esc => {
                state.search_active = false;
                state.search_query.clear();
                state.refresh_prompts()?;
                Ok(KeyResult::Continue)
            }
            KeyCode::Enter => {
                state.search_active = false;
                Ok(KeyResult::Continue)
            }
            KeyCode::Char(c) => {
                state.search_query.push(c);
                state.refresh_prompts()?;
                Ok(KeyResult::Continue)
            }
            KeyCode::Backspace => {
                state.search_query.pop();
                state.refresh_prompts()?;
                Ok(KeyResult::Continue)
            }
            _ => Ok(KeyResult::Continue),
        }
    } else {
        // Normal navigation mode
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.previous();
                Ok(KeyResult::Continue)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.next()?;
                Ok(KeyResult::Continue)
            }
            KeyCode::Left | KeyCode::Char('h') => {
                state.previous_tab()?;
                Ok(KeyResult::Continue)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                state.next_tab()?;
                Ok(KeyResult::Continue)
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                state.preview_mode = true;
                state.preview_scroll = 0;
                Ok(KeyResult::Continue)
            }
            KeyCode::Char('/') => {
                state.search_active = true;
                Ok(KeyResult::Continue)
            }
            KeyCode::Enter => Ok(KeyResult::Select),
            KeyCode::Esc | KeyCode::Char('q') => Ok(KeyResult::Exit),
            _ => Ok(KeyResult::Continue),
        }
    }
}

fn render(f: &mut Frame, state: &PromptPickerState) {
    // If in preview mode, render full-page preview instead of list
    if state.preview_mode {
        render_preview_page(f, state);
        return;
    }

    // Main layout: [Title 1] [Tabs 3] [Search 3] [List Min10] [Footer 3]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Title
            Constraint::Length(3), // Tabs
            Constraint::Length(3), // Search
            Constraint::Min(10),   // List
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Render title
    let title = Paragraph::new(state.title.clone())
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Render tabs
    render_tabs(f, chunks[1], state);

    // Render search
    render_search(f, chunks[2], state);

    // Render prompt list
    render_prompt_list(f, chunks[3], state);

    // Render footer
    render_footer(f, chunks[4], state);
}

fn render_tabs(f: &mut Frame, area: Rect, state: &PromptPickerState) {
    let tab_titles = if state.current_workdir.is_some() {
        vec!["Current Repo", "All"]
    } else {
        vec!["All"]
    };

    let selected_tab = match state.current_tab {
        Tab::CurrentRepo => 0,
        Tab::All => {
            if state.current_workdir.is_some() {
                1
            } else {
                0
            }
        }
    };

    let tabs = Tabs::new(tab_titles)
        .block(Block::default().borders(Borders::ALL).title("View"))
        .select(selected_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(tabs, area);
}

fn render_search(f: &mut Frame, area: Rect, state: &PromptPickerState) {
    let search_text = if state.search_active {
        format!("Search: {}_", state.search_query)
    } else if state.search_query.is_empty() {
        "Search: (press / to search)".to_string()
    } else {
        format!("Search: {} (press / to edit)", state.search_query)
    };

    let search = Paragraph::new(search_text)
        .block(Block::default().borders(Borders::ALL))
        .style(if state.search_active {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Gray)
        });

    f.render_widget(search, area);
}

fn render_prompt_list(f: &mut Frame, area: Rect, state: &PromptPickerState) {
    if state.prompts.is_empty() {
        let msg = if state.search_query.is_empty() {
            "No prompts found in database"
        } else {
            "No prompts match your search"
        };
        let empty = Paragraph::new(msg)
            .block(Block::default().borders(Borders::ALL).title("Prompts"))
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Gray));
        f.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = state
        .prompts
        .iter()
        .map(|prompt| {
            let snippet = prompt.first_message_snippet(80);
            let time = prompt.relative_time();
            let count = prompt.message_count();

            let info_line = format!("  {} · {} messages", time, count);

            let lines = vec![
                Line::from(snippet),
                Line::from(Span::styled(
                    info_line,
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            ListItem::new(Text::from(lines))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Prompts ({} loaded)", state.prompts.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_index));

    f.render_stateful_widget(list, area, &mut list_state);
}

fn render_footer(f: &mut Frame, area: Rect, state: &PromptPickerState) {
    let help_text = if state.preview_mode {
        "Enter: Select | Esc: Close Preview"
    } else if state.search_active {
        "Type to search | Enter: Done | Esc: Cancel"
    } else {
        "↑↓/jk: Navigate | ←→/hl: Tabs | /: Search | P: Preview | Enter: Select | Esc/q: Exit"
    };

    let footer = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Cyan))
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

fn format_messages_for_display(
    transcript: &crate::authorship::transcript::AiTranscript,
) -> Vec<Line<'_>> {
    use crate::authorship::transcript::Message;

    let mut all_lines: Vec<Line> = Vec::new();

    // Iterate through messages (oldest first, newest last as per user request)
    for message in &transcript.messages {
        match message {
            Message::User { text, .. } => {
                all_lines.push(Line::from(Span::styled(
                    format!("User: {}", text),
                    Style::default().fg(Color::Cyan),
                )));
            }
            Message::Assistant { text, .. } => {
                all_lines.push(Line::from(Span::styled(
                    format!("Assistant: {}", text),
                    Style::default().fg(Color::Green),
                )));
            }
            Message::Thinking { text, .. } => {
                all_lines.push(Line::from(Span::styled(
                    format!("Thinking: {}", text),
                    Style::default().fg(Color::Magenta),
                )));
            }
            Message::Plan { text, .. } => {
                all_lines.push(Line::from(Span::styled(
                    format!("Plan: {}", text),
                    Style::default().fg(Color::Blue),
                )));
            }
            Message::ToolUse { name, .. } => {
                all_lines.push(Line::from(Span::styled(
                    format!("Tool: {}", name),
                    Style::default().fg(Color::Yellow),
                )));
            }
        }
        // Add blank line between messages
        all_lines.push(Line::from(""));
    }

    all_lines
}

fn render_preview_page(f: &mut Frame, state: &PromptPickerState) {
    let Some(prompt) = state.get_selected() else {
        return;
    };

    // Full-page layout: [Header 5] [Messages Min(10)] [Footer 3]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // Header with metadata
            Constraint::Min(10),   // Messages
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Render header with prompt metadata
    let header_text = vec![
        Line::from(Span::styled(
            format!("ID: {}", prompt.id),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            format!("Tool: {} | Model: {}", prompt.tool, prompt.model),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            format!(
                "Messages: {} | Created: {}",
                prompt.message_count(),
                prompt.relative_time()
            ),
            Style::default().fg(Color::Cyan),
        )),
    ];

    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL).title("Preview"))
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(header, chunks[0]);

    // Render messages
    let message_lines = format_messages_for_display(&prompt.messages);

    let messages_widget = Paragraph::new(message_lines)
        .block(Block::default().borders(Borders::ALL).title("Messages"))
        .wrap(Wrap { trim: false })
        .scroll((state.preview_scroll as u16, 0));
    f.render_widget(messages_widget, chunks[1]);

    // Render footer with help text
    let footer_text = "↑↓/jk: Scroll | Enter: Select | Esc: Back";
    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Cyan))
        .alignment(Alignment::Center);
    f.render_widget(footer, chunks[2]);
}
