use crate::authorship::internal_db::PromptDbRecord;
use crate::commands::prompt_picker;
use crate::error::GitAiError;
use crate::git::find_repository;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::io;

#[derive(Clone)]
struct ShareConfig {
    title: String,
    title_cursor: usize,
    share_all_in_commit: bool,
    include_diffs: bool,
    can_share_commit: bool,
    /// Which checkbox is focused (0 = share_all_in_commit, 1 = include_diffs)
    focused_checkbox: usize,
}

impl ShareConfig {
    fn new(prompt: &PromptDbRecord) -> Self {
        let title = prompt.first_message_snippet(60);

        let can_share_commit = prompt.commit_sha.is_some();

        Self {
            title,
            title_cursor: 0,
            share_all_in_commit: false,
            include_diffs: true,
            can_share_commit,
            focused_checkbox: 0,
        }
    }
}

pub fn run_tui() -> Result<(), GitAiError> {
    let repo = find_repository(&Vec::<String>::new()).ok();

    // Sync recent prompts before showing picker to ensure fresh data
    let _ = crate::commands::sync_prompts::sync_recent_prompts_silent(20);

    loop {
        // Step 1: Use prompt_picker to select a prompt
        let selected_prompt = prompt_picker::pick_prompt(repo.as_ref(), "Select Prompt to Share")?;

        let selected_prompt = match selected_prompt {
            Some(p) => p,
            None => return Ok(()), // User cancelled from picker
        };

        // Step 2: Show share configuration screen
        let config = show_share_config_screen(&selected_prompt)?;

        let config = match config {
            Some(c) => c,
            None => {
                // User went back - re-launch picker
                continue;
            }
        };

        // Step 3: Create and submit bundle
        let prompt_record = selected_prompt.to_prompt_record();

        let response = crate::commands::share::create_bundle(
            selected_prompt.id,
            prompt_record,
            config.title,
            config.share_all_in_commit,
            config.include_diffs,
        )?;

        // Display result
        println!("{}", response.url);

        return Ok(());
    }
}

fn show_share_config_screen(prompt: &PromptDbRecord) -> Result<Option<ShareConfig>, GitAiError> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Initialize config
    let mut config = ShareConfig::new(prompt);

    // Track which field is focused (0 = title, 1 = scope)
    let mut focused_field = 0;

    // Main event loop
    let result = loop {
        terminal.draw(|f| render_config_screen(f, &config, focused_field))?;

        if let Event::Key(key) = event::read()? {
            // Only handle key press events
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match handle_config_key_event(&mut config, &mut focused_field, key) {
                ConfigKeyResult::Continue => {}
                ConfigKeyResult::Back => break None,
                ConfigKeyResult::Submit => break Some(config.clone()),
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

enum ConfigKeyResult {
    Continue,
    Back,
    Submit,
}

fn handle_config_key_event(
    config: &mut ShareConfig,
    focused_field: &mut usize,
    key: KeyEvent,
) -> ConfigKeyResult {
    match key.code {
        KeyCode::Esc => ConfigKeyResult::Back,
        KeyCode::Tab => {
            // Cycle focus: 0 (title) -> 1 (options) -> 0
            *focused_field = (*focused_field + 1) % 2;
            ConfigKeyResult::Continue
        }
        KeyCode::BackTab => {
            // Reverse cycle
            *focused_field = if *focused_field == 0 { 1 } else { 0 };
            ConfigKeyResult::Continue
        }
        KeyCode::Enter => ConfigKeyResult::Submit,
        _ => {
            // Handle input based on focused field
            match *focused_field {
                0 => {
                    // Title editing
                    // Handle Ctrl+U to clear title
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('u')
                    {
                        config.title.clear();
                        config.title_cursor = 0;
                        return ConfigKeyResult::Continue;
                    }
                    match key.code {
                        KeyCode::Char(c) => {
                            config.title.insert(config.title_cursor, c);
                            config.title_cursor += 1;
                        }
                        KeyCode::Backspace => {
                            if config.title_cursor > 0 {
                                config.title.remove(config.title_cursor - 1);
                                config.title_cursor -= 1;
                            }
                        }
                        KeyCode::Left => {
                            if config.title_cursor > 0 {
                                config.title_cursor -= 1;
                            }
                        }
                        KeyCode::Right => {
                            if config.title_cursor < config.title.len() {
                                config.title_cursor += 1;
                            }
                        }
                        KeyCode::Home => {
                            config.title_cursor = 0;
                        }
                        KeyCode::End => {
                            config.title_cursor = config.title.len();
                        }
                        _ => {}
                    }
                }
                1 => {
                    // Checkbox section
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            // Move focus up between checkboxes
                            if config.focused_checkbox > 0 {
                                config.focused_checkbox -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            // Move focus down between checkboxes
                            if config.focused_checkbox < 1 {
                                config.focused_checkbox += 1;
                            }
                        }
                        KeyCode::Char(' ') => {
                            // Toggle focused checkbox
                            match config.focused_checkbox {
                                0 => {
                                    // Share all in commit - only toggle if can_share_commit
                                    if config.can_share_commit {
                                        config.share_all_in_commit = !config.share_all_in_commit;
                                    }
                                }
                                1 => {
                                    // Include diffs - always toggleable
                                    config.include_diffs = !config.include_diffs;
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            ConfigKeyResult::Continue
        }
    }
}

fn render_config_screen(f: &mut Frame, config: &ShareConfig, focused_field: usize) {
    // Layout: [Header 3] [Title 5] [Options 8] [Footer 3]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(5), // Title input
            Constraint::Length(8), // Options (checkboxes)
            Constraint::Min(0),    // Spacer
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Header
    let header = Paragraph::new("Share Prompt")
        .block(Block::default().borders(Borders::ALL))
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(header, chunks[0]);

    // Title input
    let title_focused = focused_field == 0;
    let title_style = if title_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    };

    let title_block = Block::default()
        .borders(Borders::ALL)
        .title("Title (Tab to switch fields)")
        .border_style(if title_focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });

    let title_text = if title_focused {
        // Show cursor
        let before = &config.title[..config.title_cursor];
        let after = &config.title[config.title_cursor..];
        format!("{}_{}", before, after)
    } else {
        config.title.clone()
    };

    let title_widget = Paragraph::new(title_text)
        .block(title_block)
        .style(title_style);

    f.render_widget(title_widget, chunks[1]);

    // Options section (checkboxes)
    let options_focused = focused_field == 1;
    let options_block = Block::default()
        .borders(Borders::ALL)
        .title("Options (↑↓ to move, Space to toggle)")
        .border_style(if options_focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });

    // Checkbox 0: Share all prompts in commit
    let commit_marker = if config.share_all_in_commit {
        "[x]"
    } else {
        "[ ]"
    };
    let commit_focused = options_focused && config.focused_checkbox == 0;
    let commit_style = if !config.can_share_commit {
        Style::default().fg(Color::DarkGray)
    } else if commit_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let commit_text = if !config.can_share_commit {
        format!("{} Share all prompts in commit (no commit)", commit_marker)
    } else {
        format!("{} Share all prompts in commit", commit_marker)
    };

    // Checkbox 1: Include code diffs
    let diffs_marker = if config.include_diffs { "[x]" } else { "[ ]" };
    let diffs_focused = options_focused && config.focused_checkbox == 1;
    let diffs_style = if diffs_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let diffs_text = format!("{} Include code diffs", diffs_marker);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(commit_text, commit_style)),
        Line::from(""),
        Line::from(Span::styled(diffs_text, diffs_style)),
    ];

    let options_widget = Paragraph::new(lines).block(options_block);

    f.render_widget(options_widget, chunks[2]);

    // Footer
    let footer = Paragraph::new("Tab: Next field | Space: Toggle | Enter: Submit | Esc: Back")
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Cyan))
        .alignment(Alignment::Center);

    f.render_widget(footer, chunks[4]);
}
