//! Ratatui TUI for `git-ai activity`.

use crate::error::GitAiError;
use crate::metrics::local_stats::{
    BucketGranularity, LocalActivityStats, RepoActivitySummary, compute_activity,
    compute_repo_summaries,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Bar, BarChart, Block, Cell, Clear, Gauge, Paragraph, Row, Sparkline, Table, Tabs},
};
use std::time::{SystemTime, UNIX_EPOCH};

const TAB_NAMES: &[&str] = &["Summary", "Models"];

struct Period {
    label: &'static str,
    granularity: BucketGranularity,
    days: Option<u64>, // None = all time
}

const PERIODS: &[Period] = &[
    Period {
        label: "last 1 day",
        granularity: BucketGranularity::Daily,
        days: Some(1),
    },
    Period {
        label: "last 3 days",
        granularity: BucketGranularity::Daily,
        days: Some(3),
    },
    Period {
        label: "last 7 days",
        granularity: BucketGranularity::Daily,
        days: Some(7),
    },
    Period {
        label: "last 30 days",
        granularity: BucketGranularity::Weekly,
        days: Some(30),
    },
    Period {
        label: "all time",
        granularity: BucketGranularity::Monthly,
        days: None,
    },
];

fn since_ts(period_idx: usize) -> u32 {
    match PERIODS[period_idx].days {
        None => 0,
        Some(days) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.saturating_sub(days * 24 * 3600) as u32
        }
    }
}

struct AppState {
    selected_tab: usize,
    period_idx: usize,
    stats: LocalActivityStats,
    /// The repo URL we're scoped to, or None for global (all repos) view.
    current_repo: Option<String>,
    /// Per-repo summaries; only populated when `current_repo` is None.
    repo_summaries: Vec<RepoActivitySummary>,
}

impl AppState {
    fn new(
        stats: LocalActivityStats,
        period_idx: usize,
        current_repo: Option<String>,
        repo_summaries: Vec<RepoActivitySummary>,
    ) -> Self {
        Self {
            selected_tab: 0,
            period_idx,
            stats,
            current_repo,
            repo_summaries,
        }
    }

    fn load_period(&mut self, idx: usize) -> Result<(), GitAiError> {
        let p = &PERIODS[idx];
        let ts = since_ts(idx);
        // Always pass None — see activity.rs comment about NULL repo_url on
        // historical events.  current_repo is display-only.
        self.stats = compute_activity(ts, p.label.to_string(), p.granularity, None)?;
        if self.current_repo.is_none() {
            self.repo_summaries = compute_repo_summaries(ts, p.granularity).unwrap_or_default();
        }
        self.period_idx = idx;
        Ok(())
    }
}

pub fn run_tui(
    initial_stats: LocalActivityStats,
    period_idx: usize,
    current_repo: Option<String>,
    repo_summaries: Vec<RepoActivitySummary>,
) -> Result<(), GitAiError> {
    let mut terminal = ratatui::init();
    let result = run_app(
        &mut terminal,
        initial_stats,
        period_idx,
        current_repo,
        repo_summaries,
    );
    ratatui::restore();
    result
}

fn run_app(
    terminal: &mut DefaultTerminal,
    initial_stats: LocalActivityStats,
    period_idx: usize,
    current_repo: Option<String>,
    repo_summaries: Vec<RepoActivitySummary>,
) -> Result<(), GitAiError> {
    let mut app = AppState::new(initial_stats, period_idx, current_repo, repo_summaries);
    loop {
        terminal
            .draw(|frame| render(frame, &app))
            .map_err(GitAiError::IoError)?;

        if let Event::Key(key) = event::read().map_err(GitAiError::IoError)? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                    app.selected_tab = (app.selected_tab + 1) % TAB_NAMES.len();
                }
                KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                    app.selected_tab = (app.selected_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len();
                }
                KeyCode::Char(c @ '1'..='5') => {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < PERIODS.len() {
                        app.load_period(idx)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

// ─── Top-level render ────────────────────────────────────────────────────────

fn render(frame: &mut Frame, app: &AppState) {
    // Clear any leftover content from behind the TUI.
    frame.render_widget(Clear, frame.area());

    // Outer padding: 1 row top/bottom, 2 cols left/right.
    let [_, padded_v, _] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [_, padded, _] = Layout::horizontal([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(padded_v);

    let [header_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(padded);

    render_header(frame, header_area, app);
    render_footer(frame, footer_area);

    match app.selected_tab {
        0 => render_summary(frame, content_area, app),
        1 => render_models(frame, content_area, app),
        _ => {}
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &AppState) {
    let [title_area, tabs_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let period = PERIODS[app.period_idx].label;
    let mut title_spans = vec![
        Span::from("git-ai activity").bold(),
        Span::from("  ─  ").dim(),
        Span::from(period).dim(),
    ];
    if let Some(repo) = &app.current_repo {
        title_spans.push(Span::from("  ─  ").dim());
        title_spans.push(Span::from(repo.clone()).dim());
    }
    let title = Line::from(title_spans);
    frame.render_widget(title, title_area);

    let tabs = Tabs::new(TAB_NAMES.to_vec())
        .select(app.selected_tab)
        .style(Style::default().dim())
        .highlight_style(Style::default().bold().add_modifier(Modifier::REVERSED))
        .divider("  ")
        .padding(" ", " ");
    frame.render_widget(tabs, tabs_area);
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let footer = Line::from(vec![
        Span::from("tab/←/→").bold(),
        Span::from(": navigate  ").dim(),
        Span::from("1-5").bold(),
        Span::from(": period (1d 3d 7d 30d all)  ").dim(),
        Span::from("q").bold(),
        Span::from(": quit").dim(),
    ]);
    frame.render_widget(footer, area);
}

// ─── Summary tab ─────────────────────────────────────────────────────────────
//
// Layout (top → bottom):
//   [4 stat boxes]
//   [AI lines bar chart — fills remaining height]
//   [session / yield / acceptance stats line]
//   [Time of day  |  Day of week heatmaps]

fn render_summary(frame: &mut Frame, area: Rect, app: &AppState) {
    let stats = &app.stats;
    let has_hourly = stats.hourly.iter().any(|&v| v > 0);
    let has_daily = stats.daily.iter().any(|&v| v > 0);
    let heatmap_height = if has_hourly || has_daily { 7u16 } else { 0 };

    let [stat_area, chart_area, session_area, heatmap_area] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Length(heatmap_height),
    ])
    .areas(area);

    render_stat_boxes(frame, stat_area, stats);

    // When not scoped to a repo, show a per-repo breakdown in place of the
    // activity chart.  When scoped, show the normal AI-lines bar chart.
    if app.current_repo.is_none() && !app.repo_summaries.is_empty() {
        render_repo_table(frame, chart_area, &app.repo_summaries);
    } else {
        render_activity_chart(frame, chart_area, stats);
    }

    render_session_stats(frame, session_area, stats);
    if heatmap_height > 0 {
        render_heatmaps(frame, heatmap_area, stats, has_hourly, has_daily);
    }
}

fn render_stat_boxes(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let total = stats.commits.ai_lines + stats.commits.human_lines;
    let ai_pct = (stats.commits.ai_lines * 100)
        .checked_div(total)
        .unwrap_or(0);
    let cost = stats.tokens.estimated_cost_usd;

    let [lines_area, ai_area, sessions_area, cost_area] = Layout::horizontal([
        Constraint::Length(18),
        Constraint::Fill(1),
        Constraint::Length(20),
        Constraint::Length(14),
    ])
    .areas(area);

    // Total lines box
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::from("Total lines").dim()),
            Line::from(Span::from(fmt_num(total as u64)).bold()),
        ])
        .block(Block::bordered()),
        lines_area,
    );

    // AI share gauge
    let gauge = Gauge::default()
        .block(Block::bordered().title(Span::from("AI share").dim()))
        .ratio(if total > 0 {
            ai_pct as f64 / 100.0
        } else {
            0.0
        })
        .label(format!(
            "{}%  ·  {} AI / {} human",
            ai_pct,
            fmt_k(stats.commits.ai_lines as u64),
            fmt_k(stats.commits.human_lines as u64),
        ))
        .gauge_style(Style::default().fg(Color::Cyan));
    frame.render_widget(gauge, ai_area);

    // Sessions box (replaces old "Models used" — more useful at a glance)
    let yield_total = stats.sessions.yield_stats.shipped + stats.sessions.yield_stats.abandoned;
    let yield_pct = (stats.sessions.yield_stats.shipped * 100)
        .checked_div(yield_total)
        .unwrap_or(0);
    let sessions_label = if yield_total > 0 {
        format!(
            "{}  ({}% shipped)",
            fmt_num(stats.sessions.total as u64),
            yield_pct
        )
    } else {
        fmt_num(stats.sessions.total as u64)
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::from("Sessions").dim()),
            Line::from(Span::from(sessions_label).bold()),
        ])
        .block(Block::bordered()),
        sessions_area,
    );

    // Est. cost box
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::from("Est. cost").dim()),
            Line::from(
                Span::from(if cost > 0.0 {
                    format!("~${:.2}", cost)
                } else {
                    "—".to_string()
                })
                .bold(),
            ),
        ])
        .block(Block::bordered()),
        cost_area,
    );
}

fn render_session_stats(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let yield_total = stats.sessions.yield_stats.shipped + stats.sessions.yield_stats.abandoned;
    let yield_pct = (stats.sessions.yield_stats.shipped * 100)
        .checked_div(yield_total)
        .unwrap_or(0);
    let accept_pct = (stats.commits.ai_lines * 100)
        .checked_div(stats.checkpoints.ai_lines_added)
        .filter(|&p| p <= 100);

    let mut spans = vec![
        Span::from("Sessions: ").dim(),
        Span::from(fmt_num(stats.sessions.total as u64)).bold(),
        Span::from("  ·  Shipped: ").dim(),
        Span::from(fmt_num(stats.sessions.yield_stats.shipped as u64)).bold(),
        Span::from(format!(" ({}%)", yield_pct)).dim(),
        Span::from("  ·  Commits: ").dim(),
        Span::from(fmt_num(stats.commits.total as u64)).bold(),
    ];
    if let Some(pct) = accept_pct {
        spans.push(Span::from("  ·  Accept rate: ").dim());
        spans.push(Span::from(format!("{}%", pct)).bold());
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_heatmaps(
    frame: &mut Frame,
    area: Rect,
    stats: &LocalActivityStats,
    has_hourly: bool,
    has_daily: bool,
) {
    match (has_hourly, has_daily) {
        (true, true) => {
            let [left, right] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(area);
            render_time_of_day(frame, left, stats);
            render_day_of_week(frame, right, stats);
        }
        (true, false) => render_time_of_day(frame, area, stats),
        (false, true) => render_day_of_week(frame, area, stats),
        _ => {}
    }
}

fn render_time_of_day(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let block = Block::bordered().title(Span::from("Time of day").bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let max_val = stats.hourly.iter().copied().max().unwrap_or(1).max(1) as u64;
    let data: Vec<u64> = stats.hourly.iter().map(|&v| v as u64).collect();
    let [spark_area, label_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .max(max_val)
            .style(Style::default().fg(Color::Cyan)),
        spark_area,
    );
    frame.render_widget(
        Paragraph::new(
            Span::from("am  1  2  3  4  5  6  7  8  9 10 11 pm  1  2  3  4  5  6  7  8  9 10 11")
                .dim(),
        ),
        label_area,
    );
}

fn render_day_of_week(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let block = Block::bordered().title(Span::from("Day of week").bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let max_val = stats.daily.iter().copied().max().unwrap_or(1).max(1) as u64;
    let data: Vec<u64> = stats.daily.iter().map(|&v| v as u64).collect();
    let [spark_area, label_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .max(max_val)
            .style(Style::default().fg(Color::Cyan)),
        spark_area,
    );
    frame.render_widget(
        Paragraph::new(Span::from("Mon   Tue   Wed   Thu   Fri   Sat   Sun").dim()),
        label_area,
    );
}

// ─── Models tab ──────────────────────────────────────────────────────────────
//
// Layout (top → bottom):
//   [Spend summary: total cost + WoW delta]
//   [Cache hit rate gauge]
//   [Model table: Model | Sessions | Tokens | Cost | Cache hit]

fn render_models(frame: &mut Frame, area: Rect, app: &AppState) {
    let stats = &app.stats;
    let t = &stats.tokens;
    let has_token_data = t.input + t.output + t.cache_read + t.cache_creation > 0;

    let spend_height = if has_token_data { 4u16 } else { 0 };
    let gauge_height = if has_token_data { 3u16 } else { 0 };

    let [spend_area, gauge_area, table_area] = Layout::vertical([
        Constraint::Length(spend_height),
        Constraint::Length(gauge_height),
        Constraint::Fill(1),
    ])
    .areas(area);

    if has_token_data {
        render_spend_summary(frame, spend_area, stats);
        render_cache_gauge(frame, gauge_area, stats);
    }
    render_model_table(frame, table_area, stats);
}

fn render_spend_summary(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let t = &stats.tokens;
    let block = Block::bordered().title(Span::from("Spend").bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cost_line = if t.estimated_cost_usd > 0.0 {
        format!("Est. cost: ~${:.2}", t.estimated_cost_usd)
    } else {
        "No cost data".to_string()
    };

    let wow_line = t.wow_spend.as_ref().map(|w| {
        let delta = match (w.new_this_week, w.change_pct) {
            (true, _) => "↑ new this week".to_string(),
            (_, Some(p)) if p > 0.0 => format!("↑ {:.0}% vs last week", p),
            (_, Some(p)) if p < 0.0 => format!("↓ {:.0}% vs last week", p.abs()),
            _ => "→ no change vs last week".to_string(),
        };
        let last_week = if w.last_week_usd > 0.01 {
            format!("  ·  Last week: ~${:.2}", w.last_week_usd)
        } else {
            String::new()
        };
        format!(
            "This week: ~${:.2}{}  {}",
            w.this_week_usd, last_week, delta
        )
    });

    let mut lines = vec![Line::from(Span::from(cost_line).bold())];
    if let Some(w) = wow_line {
        lines.push(Line::from(Span::from(w).dim()));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_cache_gauge(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let t = &stats.tokens;
    let with_ratio: Vec<f64> = t
        .by_model
        .iter()
        .filter_map(|m| m.cache_hit_ratio)
        .collect();
    let cache_hit_ratio = if with_ratio.is_empty() {
        None
    } else {
        Some(with_ratio.iter().sum::<f64>() / with_ratio.len() as f64)
    };

    let gauge = Gauge::default()
        .block(Block::bordered().title(Span::from("Cache hit rate").bold()))
        .ratio(cache_hit_ratio.unwrap_or(0.0))
        .label(
            cache_hit_ratio
                .map(|r| format!("{:.0}%", r * 100.0))
                .unwrap_or_else(|| "—".to_string()),
        )
        .gauge_style(Style::default().fg(Color::Green));
    frame.render_widget(gauge, area);
}

fn render_model_table(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let block = Block::bordered().title(Span::from("Models").bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if stats.tokens.by_model.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::from("No model data for this period.").dim()),
            inner,
        );
        return;
    }

    let total_tokens: u64 = stats
        .tokens
        .by_model
        .iter()
        .map(|m| m.input + m.output + m.cache_read + m.cache_creation)
        .sum();

    let header = Row::new(vec!["Model", "Sessions", "Tokens", "Cost", "Cache hit"])
        .style(Style::default().bold())
        .bottom_margin(1);

    let rows: Vec<Row> = stats
        .tokens
        .by_model
        .iter()
        .map(|m| {
            let tokens = m.input + m.output + m.cache_read + m.cache_creation;
            let pct = (tokens * 100).checked_div(total_tokens).unwrap_or(0);
            let sessions = stats
                .sessions
                .by_tool
                .iter()
                .find(|(tool, _)| tool.contains(&m.model))
                .map(|(_, n)| *n)
                .unwrap_or(0);
            Row::new(vec![
                Cell::from(m.model.clone()),
                Cell::from(if sessions > 0 {
                    fmt_num(sessions as u64)
                } else {
                    "—".to_string()
                }),
                Cell::from(format!("{}  ({}%)", fmt_num_tokens(tokens), pct)),
                Cell::from(
                    m.estimated_cost_usd
                        .map(|c| format!("~${:.2}", c))
                        .unwrap_or_else(|| "—".to_string()),
                ),
                Cell::from(
                    m.cache_hit_ratio
                        .map(|r| format!("{:.0}%", r * 100.0))
                        .unwrap_or_else(|| "—".to_string()),
                ),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(18),
            Constraint::Length(10),
            Constraint::Length(10),
        ],
    )
    .header(header);
    frame.render_widget(table, inner);
}

// ─── Per-repository breakdown table ──────────────────────────────────────────

fn render_repo_table(frame: &mut Frame, area: Rect, repos: &[RepoActivitySummary]) {
    let block = Block::bordered().title(Span::from("Activity by repository").bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let header = Row::new(vec![
        "Repository",
        "AI Lines",
        "Commits",
        "Sessions",
        "Est. Cost",
    ])
    .style(Style::default().bold())
    .bottom_margin(1);

    let total_ai: u32 = repos.iter().map(|r| r.ai_lines).sum();

    let rows: Vec<Row> = repos
        .iter()
        .map(|r| {
            let pct = (r.ai_lines as u64 * 100)
                .checked_div(total_ai as u64)
                .unwrap_or(0);
            let repo_display = shorten_repo_url(&r.repo_url);
            let cost = if r.estimated_cost_usd > 0.0 {
                format!("~${:.2}", r.estimated_cost_usd)
            } else {
                "—".to_string()
            };
            Row::new(vec![
                Cell::from(repo_display.to_string()),
                Cell::from(format!("{}  ({}%)", fmt_num(r.ai_lines as u64), pct)),
                Cell::from(fmt_num(r.commits as u64)),
                Cell::from(fmt_num(r.sessions as u64)),
                Cell::from(cost),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(18),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(12),
        ],
    )
    .header(header);
    frame.render_widget(table, inner);
}

// ─── Activity bar chart ───────────────────────────────────────────────────────

fn render_activity_chart(frame: &mut Frame, area: Rect, stats: &LocalActivityStats) {
    let block = Block::bordered().title(Span::from("AI lines over time").bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if stats.buckets.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::from("No activity data for this period.").dim()),
            inner,
        );
        return;
    }

    let bars: Vec<Bar> = stats
        .buckets
        .iter()
        .map(|b| {
            Bar::with_label(shorten_label(&b.label), b.ai_lines as u64)
                .style(Style::default().fg(Color::Cyan))
        })
        .collect();

    frame.render_widget(BarChart::vertical(bars).bar_width(4).bar_gap(1), inner);
}

// ─── Formatting helpers ───────────────────────────────────────────────────────

fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

fn fmt_k(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn fmt_num_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn shorten_label(label: &str) -> &str {
    if label.len() <= 6 { label } else { &label[..6] }
}

/// Strip leading `https://` / `http://` from a repo URL for compact display.
fn shorten_repo_url(url: &str) -> &str {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
}
