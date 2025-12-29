use crate::authorship::internal_db::PromptDbRecord;
use crate::commands::prompt_picker;
use crate::error::GitAiError;
use crate::git::find_repository;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShareScope {
    SinglePrompt,
    AllInCommit,
    AllInPR,
}

#[derive(Clone)]
struct ShareConfig {
    title: String,
    title_cursor: usize,
    scope_selection: ShareScope,
    can_share_commit: bool,
}

impl ShareConfig {
    fn new(prompt: &PromptDbRecord) -> Self {
        let title = format!(
            "{} ({})",
            prompt.first_message_snippet(60),
            prompt.tool
        );

        let can_share_commit = prompt.commit_sha.is_some();

        Self {
            title,
            title_cursor: 0,
            scope_selection: ShareScope::SinglePrompt,
            can_share_commit,
        }
    }
}

pub fn run_tui() -> Result<(), GitAiError> {
    let repo = find_repository(&Vec::<String>::new()).ok();

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
        let include_all_in_commit = config.scope_selection == ShareScope::AllInCommit;

        // Validate "All in PR" not implemented
        if config.scope_selection == ShareScope::AllInPR {
            eprintln!("Error: PR bundles are not yet implemented");
            std::process::exit(1);
        }

        let prompt_record = selected_prompt.to_prompt_record();

        let response = crate::commands::share::create_bundle(
            selected_prompt.id,
            prompt_record,
            config.title,
            include_all_in_commit,
        )?;

        // Display result
        println!("Bundle created successfully!");
        println!("ID: {}", response.id);
        println!("URL: {}", response.url);

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
            // Cycle focus: 0 (title) -> 1 (scope) -> 0
            *focused_field = (*focused_field + 1) % 2;
            ConfigKeyResult::Continue
        }
        KeyCode::BackTab => {
            // Reverse cycle
            *focused_field = if *focused_field == 0 { 1 } else { 0 };
            ConfigKeyResult::Continue
        }
        KeyCode::Enter => {
            // Validate and submit
            if config.scope_selection == ShareScope::AllInPR {
                // Show error but don't submit - handled in main loop
            }
            ConfigKeyResult::Submit
        }
        _ => {
            // Handle input based on focused field
            match *focused_field {
                0 => {
                    // Title editing
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
                    // Scope selection
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            config.scope_selection = match config.scope_selection {
                                ShareScope::SinglePrompt => ShareScope::SinglePrompt,
                                ShareScope::AllInCommit => ShareScope::SinglePrompt,
                                ShareScope::AllInPR => {
                                    if config.can_share_commit {
                                        ShareScope::AllInCommit
                                    } else {
                                        ShareScope::SinglePrompt
                                    }
                                }
                            };
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            config.scope_selection = match config.scope_selection {
                                ShareScope::SinglePrompt => {
                                    if config.can_share_commit {
                                        ShareScope::AllInCommit
                                    } else {
                                        ShareScope::AllInPR
                                    }
                                }
                                ShareScope::AllInCommit => ShareScope::AllInPR,
                                ShareScope::AllInPR => ShareScope::AllInPR,
                            };
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
    // Layout: [Header 3] [Title 5] [Scope 12] [Footer 3]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Length(5),  // Title input
            Constraint::Length(12), // Scope selection
            Constraint::Min(0),     // Spacer
            Constraint::Length(3),  // Footer
        ])
        .split(f.area());

    // Header
    let header = Paragraph::new("Share Prompt")
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
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

    // Scope selection
    let scope_focused = focused_field == 1;
    let scope_block = Block::default()
        .borders(Borders::ALL)
        .title("Scope (↑↓ to select)")
        .border_style(if scope_focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });

    let single_selected = config.scope_selection == ShareScope::SinglePrompt;
    let commit_selected = config.scope_selection == ShareScope::AllInCommit;
    let pr_selected = config.scope_selection == ShareScope::AllInPR;

    let single_style = if single_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let commit_style = if !config.can_share_commit {
        Style::default().fg(Color::DarkGray)
    } else if commit_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let pr_style = if pr_selected {
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let single_marker = if single_selected { "(*)" } else { "( )" };
    let commit_marker = if commit_selected { "(*)" } else { "( )" };
    let pr_marker = if pr_selected { "(*)" } else { "( )" };

    let commit_text = if !config.can_share_commit {
        format!("{} All prompts in commit (disabled - no commit)", commit_marker)
    } else {
        format!("{} All prompts in commit", commit_marker)
    };

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("{} Only this prompt", single_marker), single_style)),
        Line::from(""),
        Line::from(Span::styled(commit_text, commit_style)),
        Line::from(""),
        Line::from(Span::styled(
            format!("{} All prompts in PR (not implemented)", pr_marker),
            pr_style,
        )),
    ];

    let scope_widget = Paragraph::new(lines).block(scope_block);

    f.render_widget(scope_widget, chunks[2]);

    // Footer
    let footer_text = if config.scope_selection == ShareScope::AllInPR {
        "PR bundles not yet implemented - press Esc to go back"
    } else {
        "Tab: Next field | Enter: Submit | Esc: Back"
    };

    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::ALL))
        .style(if config.scope_selection == ShareScope::AllInPR {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Cyan)
        })
        .alignment(Alignment::Center);

    f.render_widget(footer, chunks[4]);
}
