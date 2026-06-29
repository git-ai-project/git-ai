//! `git-ai analyze` — query Git AI's backend analytics (Cube semantic layer)
//! and pull/grade coding sessions at scale.
//!
//! This command is the agent-facing replacement for the old local `prompts.db`
//! flow. Prompts now live in the backend; `analyze` wraps the Cube API (so an
//! agent uses it instead of `curl`) and provides a session-grading pipeline
//! backed by a scratch SQLite DB.

mod cube;
mod sessions;

use cube::{CubeClient, QueryArgs};
use serde_json::Value;

pub fn handle_analyze(args: &[String]) {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };

    // `--help`/`-h` anywhere in the args prints help, exactly like running the
    // command raw — except for `sessions`, which we delegate to so its own
    // (more specific) help is shown.
    if sub != "sessions" && args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let result = match sub {
        "" | "help" | "--help" | "-h" => {
            print_help();
            return;
        }
        "query" => cmd_query(rest),
        "docs" => cmd_docs(rest),
        "sessions" => {
            sessions::handle_sessions(rest);
            return;
        }
        other => Err(format!(
            "unknown analyze subcommand: {}\nRun `git-ai analyze` for help.",
            other
        )),
    };

    if let Err(msg) = result {
        eprintln!("Error: {}", msg);
        std::process::exit(1);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Json,
    Tsv,
    Raw,
}

fn cmd_query(args: &[String]) -> Result<(), String> {
    let mut q = QueryArgs::default();
    let mut format = Format::Json;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--measures" | "-m" => {
                q.measures
                    .extend(split_csv(&take_value(args, &mut i, "--measures")?));
            }
            "--dimensions" | "-d" => {
                q.dimensions
                    .extend(split_csv(&take_value(args, &mut i, "--dimensions")?));
            }
            "--time-dimension" | "--td" => {
                q.time_dimension = Some(take_value(args, &mut i, "--time-dimension")?);
            }
            "--granularity" | "-g" => {
                q.granularity = Some(take_value(args, &mut i, "--granularity")?);
            }
            "--since" | "--date-range" => {
                q.date_range = Some(take_value(args, &mut i, "--since")?);
            }
            "--filters" | "-f" => {
                q.filters_json = Some(take_value(args, &mut i, "--filters")?);
            }
            "--order" | "-o" => {
                q.order
                    .push(parse_order(&take_value(args, &mut i, "--order")?)?);
            }
            "--limit" | "-l" => {
                q.limit = Some(
                    take_value(args, &mut i, "--limit")?
                        .parse()
                        .map_err(|_| "--limit must be a number".to_string())?,
                );
            }
            "--offset" => {
                q.offset = Some(
                    take_value(args, &mut i, "--offset")?
                        .parse()
                        .map_err(|_| "--offset must be a number".to_string())?,
                );
            }
            "--tsv" => format = Format::Tsv,
            "--raw" => format = Format::Raw,
            "--json" => format = Format::Json,
            "--format" => {
                format = match take_value(args, &mut i, "--format")?.as_str() {
                    "json" => Format::Json,
                    "tsv" => Format::Tsv,
                    "raw" => Format::Raw,
                    other => return Err(format!("unknown --format: {} (json|tsv|raw)", other)),
                };
            }
            // `--help`/`-h` is handled by `handle_analyze` before dispatch.
            other => return Err(format!("unknown query flag: {}", other)),
        }
        i += 1;
    }

    if q.measures.is_empty() && q.dimensions.is_empty() && q.time_dimension.is_none() {
        return Err(
            "nothing to query: pass at least --measures, --dimensions, or --time-dimension \
             (try `git-ai analyze docs` to discover members)"
                .to_string(),
        );
    }

    let query = q.to_query()?;
    let client = CubeClient::from_config().map_err(|e| e.to_string())?;

    let response = client.load(&query).map_err(|e| e.to_string())?;

    render(&response, format);
    Ok(())
}

fn cmd_docs(_args: &[String]) -> Result<(), String> {
    // `--help`/`-h` is handled by `handle_analyze` before dispatch.
    let client = CubeClient::from_config().map_err(|e| e.to_string())?;
    let meta = client.meta().map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&meta).unwrap_or_default()
    );
    Ok(())
}

/// Render a `/load` response per the requested format.
fn render(response: &Value, format: Format) {
    match format {
        Format::Raw => {
            println!(
                "{}",
                serde_json::to_string_pretty(response).unwrap_or_default()
            );
        }
        Format::Json => {
            let data = response.get("data").cloned().unwrap_or(Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&data).unwrap_or_default()
            );
        }
        Format::Tsv => {
            let empty = Vec::new();
            let rows = response
                .get("data")
                .and_then(|d| d.as_array())
                .unwrap_or(&empty);
            print!("{}", cube::rows_to_tsv(rows));
        }
    }
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Parse `--order member[:dir]` into (member, direction). Defaults to `asc`.
fn parse_order(s: &str) -> Result<(String, String), String> {
    let (member, dir) = match s.split_once(':') {
        Some((m, d)) => (m.trim(), d.trim().to_lowercase()),
        None => (s.trim(), "asc".to_string()),
    };
    if member.is_empty() {
        return Err("--order requires a member name".to_string());
    }
    if dir != "asc" && dir != "desc" {
        return Err(format!(
            "--order direction must be asc or desc, got: {}",
            dir
        ));
    }
    Ok((member.to_string(), dir))
}

pub(super) fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{} requires a value", flag))
}

fn print_help() {
    let help = r#"git-ai analyze - research AI coding activity and analyze prompts at scale

You are an analyst. Your job is to answer questions about how AI is used to write
code in this org — prompting patterns, acceptance/productivity, model and agent
comparisons, who/what/when — using real data from Git AI's backend. Don't guess or
generalize: pull the actual sessions and read the transcripts.

Assume Git AI attribution is correct and what you're seeing reflects actual
performance of agents — do not explain anomalies away as instrumentation or
attribution gaps.

Two modes, and you'll usually use both:
  1. RESEARCH with aggregate queries. Use `query` to slice the warehouse (counts,
     rates, lines, breakdowns by agent/model/repo/time). Start broad to find where
     the interesting signal is — which agents, which time range, which outliers.
  2. ITERATE over specific sessions. Once a query points you at the population you
     care about, pull those exact sessions with `sessions pull` (carry your filters
     over: --since, --agent, --repo, --user), then walk them one at a time with
     `sessions next`, read each transcript, and write your findings back into the
     scratch DB so they can be aggregated. This is how you analyze prompts.

Typical loop: docs (discover members) → query (find the signal) → sessions pull
(grab that slice) → fan out subagents over `sessions next` (grade/categorize each
transcript) → exec a synthesis query over the columns you added. See
`git-ai analyze sessions --help` for the full pull/next/iterate workflow.

Data lives in a shared Cube.js semantic layer behind https://usegitai.com/api/cube.
Auth uses an org API key (organization:admin:read) sent as the x-api-key header;
set it via the GIT_AI_API_KEY env var or "api_key" in ~/.git-ai/config.json. Your
org is derived from the key — data is always scoped to your org.

Scoping to a specific person ("my sessions", "the work I did", "what Jane wrote"):
  Every cube keys on an opaque user_id, NOT an email — so resolve the person to a
  user_id FIRST, then filter by it. For the CURRENT user ("my"/"mine"/"that I did"),
  read their git email and look it up via public_v1_user_status (user_id +
  author_email):
    EMAIL=$(git config user.email)
    git-ai analyze query -d public_v1_user_status.user_id,public_v1_user_status.author_email \
      -f '[{"member":"public_v1_user_status.author_email","operator":"equals","values":["'"$EMAIL"'"]}]'
  Take the user_id from that row and carry it everywhere: as a -d/-f on `query`, or
  as `sessions pull --user <user_id>`. For someone else, swap in their email. If the
  email returns no row, say so rather than guessing a user_id.

Usage: git-ai analyze <command> [flags]

Commands:
  query [flags]      Run a Cube query and print rows (the curl replacement)
  docs               List all cubes, measures, and dimensions (discovery)
  sessions <sub>     Pull specific sessions + transcripts and grade them at scale
                     (see `analyze sessions --help`)

query flags:
  -m, --measures a,b        Measures (fully-qualified, e.g. public_v1_sessions.total_sessions)
  -d, --dimensions c,d      Dimensions to group by
      --time-dimension M    Time dimension member (enables --since/--granularity)
  -g, --granularity G       day | week | month | … (needs --time-dimension)
      --since "<range>"     Cube dateRange, e.g. "last 30 days" or "2024-01-01,2024-02-01"
  -f, --filters '<json>'    Raw JSON array of Cube filter objects (escape hatch)
  -o, --order M[:asc|desc]  Order by a member (repeatable)
  -l, --limit N             Row limit
      --offset N            Row offset
      --format json|tsv|raw json (default, prints data array) | tsv table | raw full response

Gotchas:
  - Member names are fully qualified: public_v1_pull_requests.pull_requests (no bare names).
  - There is no .count on public_v1_pull_requests — use .pull_requests.
  - Numeric measures come back as strings in JSON; cast as needed.
  - Run `git-ai analyze docs` to discover every cube/measure/dimension.
  - Don't assume a dimension's MEANING or value distribution from its name. A
    `pr_state` dimension does not imply the cube holds non-merged rows; a cube may
    be scoped to one slice (e.g. public_v1_production_hunks is production-only by
    construction). Before you reason from a dimension, run a tiny query to SEE its
    actual values (e.g. -d <dim> -m <count> -l 20) instead of guessing — guessing
    here is the #1 way this analysis talks itself into a wrong conclusion.
  - Before reading transcripts to answer "how much committed code didn't ship",
    check the funnel MEASURES first — public_v1_sessions exposes committed →
    pr_opened → merged → production as per-session line measures, so the stage
    where work leaks is queryable in aggregate. Transcripts are only needed for
    the *why* (open vs. reverted vs. superseded). `sessions pull` even
    precomputes the gap columns; see `analyze sessions --help`.

Examples:
  git-ai analyze docs
  git-ai analyze query -m public_v1_sessions.total_sessions
  git-ai analyze query -m public_v1_pull_requests.ai_assisted_pull_requests \
      --time-dimension public_v1_pull_requests.opened_time -g month --since "last 6 months" --tsv
  git-ai analyze query -m public_v1_sessions.total_generated_lines \
      -d public_v1_sessions.agent -o public_v1_sessions.total_generated_lines:desc -l 10

Common cubes: public_v1_pull_requests, public_v1_sessions, public_v1_session_models,
  public_v1_normalized_events, public_v1_token_usage, public_v1_production_hunks,
  public_v1_ai_checkpoint_events, public_v1_repos, public_v1_user_status (+ more via docs).

Pull + analyze specific sessions/prompts (read the transcripts, grade at scale):
  `git-ai analyze sessions --help`.
"#;
    eprint!("{help}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_order_with_and_without_direction() {
        assert_eq!(
            parse_order("public_v1_sessions.total_sessions:desc").unwrap(),
            ("public_v1_sessions.total_sessions".into(), "desc".into())
        );
        assert_eq!(
            parse_order("member").unwrap(),
            ("member".into(), "asc".into())
        );
        assert!(parse_order("member:sideways").is_err());
        assert!(parse_order(":desc").is_err());
    }

    #[test]
    fn splits_csv_and_trims() {
        assert_eq!(split_csv("a, b ,c"), vec!["a", "b", "c"]);
        assert_eq!(split_csv(" , "), Vec::<String>::new());
    }

    #[test]
    fn render_json_extracts_data_array() {
        // Smoke test the data-extraction path doesn't panic on missing data.
        render(&serde_json::json!({"foo": 1}), Format::Json);
        render(&serde_json::json!({"data": [{"a": "1"}]}), Format::Tsv);
    }
}
