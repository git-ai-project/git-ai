#!/usr/bin/env python3
"""
git-ai Dashboard Generator
===========================
Generates a fully self-contained HTML dashboard from:
  - LOCAL mode (default): local git history + git-ai attribution notes.
  - DB mode (--db or DATABASE_URL env var): central Postgres / Supabase
    database populated by the GitHub Actions git-ai-collector workflow.

No repo data is sent to any external system in local mode.
In DB mode, only read queries are made to your own database.
Chart.js is loaded from jsDelivr CDN.

Usage:
    # Local mode (default)
    python3 scripts/git-ai-dashboard.py
    python3 scripts/git-ai-dashboard.py --since 30d --output my-report.html
    python3 scripts/git-ai-dashboard.py --all

    # Central DB mode
    DATABASE_URL=postgresql://... python3 scripts/git-ai-dashboard.py --db
    python3 scripts/git-ai-dashboard.py --db --db-url postgresql://...

Arguments:
    --since   <time>     Limit to commits after this time (default: 90d)
                         Formats: '7d', '30d', '1y', 'YYYY-MM-DD'
    --all                Include all commits in history (may be slow)
    --max-count <n>      Max commits to process
    --output  <file>     Output file path (default: git-ai-dashboard.html)
    --no-enrich          Skip git-ai stats lookup (faster, local mode only)
    --db                 Read from central Postgres / Supabase DB instead
    --db-url  <url>      Postgres connection URL (overrides DATABASE_URL env)
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path


# ---------------------------------------------------------------------------
# Data collection
# ---------------------------------------------------------------------------

def run(cmd: list[str]) -> tuple[str, int]:
    result = subprocess.run(cmd, capture_output=True, text=True)
    return result.stdout.strip(), result.returncode


def run_json(cmd: list[str]) -> dict | None:
    out, code = run(cmd)
    if not out or code != 0:
        return None
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return None


def get_noted_commits() -> set[str]:
    """Return set of commit SHAs that have a git-ai note."""
    out, code = run(["git", "notes", "--ref=ai", "list"])
    if code != 0 or not out:
        return set()
    noted = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2:
            noted.add(parts[1])
    return noted


_SEP = "|||GS|||"


def collect_commits(max_count: int | None, since: str | None) -> list[dict]:
    """
    Collect commits via a single `git log --shortstat` invocation.
    Returns list of commit dicts with raw git log fields.
    """
    fmt = f"%H{_SEP}%an{_SEP}%ae{_SEP}%ad{_SEP}%s"
    cmd = ["git", "log", f"--format=COMMIT:{fmt}", "--date=short", "--shortstat"]
    if max_count:
        cmd += [f"--max-count={max_count}"]
    if since:
        cmd += [f"--since={since}"]

    out, _ = run(cmd)
    commits: list[dict] = []
    current: dict | None = None

    for line in out.splitlines():
        if line.startswith("COMMIT:"):
            if current:
                commits.append(current)
            parts = line[7:].split(_SEP)
            if len(parts) < 5:
                current = None
                continue
            current = {
                "sha": parts[0],
                "author": parts[1],
                "email": parts[2],
                "date": parts[3],
                "subject": parts[4],
                "files_changed": 0,
                "insertions": 0,
                "deletions": 0,
                "ai_additions": 0,
                "human_additions": 0,
                "unknown_additions": 0,
                "mixed_additions": 0,
                "has_note": False,
                "tools": {},
            }
        elif current and " changed" in line:
            m = re.search(r"(\d+) insertion", line)
            if m:
                current["insertions"] = int(m.group(1))
            m = re.search(r"(\d+) deletion", line)
            if m:
                current["deletions"] = int(m.group(1))
            m = re.search(r"(\d+) file", line)
            if m:
                current["files_changed"] = int(m.group(1))

    if current:
        commits.append(current)
    return commits


def enrich_with_git_ai(commits: list[dict], noted: set[str]) -> None:
    """For commits that have a git-ai note, fetch detailed attribution."""
    to_enrich = [c for c in commits if c["sha"] in noted]
    total = len(to_enrich)
    if total == 0:
        return
    print(f"  Enriching {total} commit(s) with git-ai attribution data ...", file=sys.stderr)
    for i, commit in enumerate(to_enrich, 1):
        stats = run_json(["git-ai", "stats", commit["sha"], "--json"])
        if stats:
            commit["has_note"] = True
            commit["ai_additions"] = (
                stats.get("ai_additions", 0) + stats.get("total_ai_additions", 0)
            )
            commit["human_additions"] = stats.get("human_additions", 0)
            commit["unknown_additions"] = stats.get("unknown_additions", 0)
            commit["mixed_additions"] = stats.get("mixed_additions", 0)
            commit["tools"] = stats.get("tool_model_breakdown", {})
        if i % 50 == 0:
            print(f"    {i}/{total} done ...", file=sys.stderr)


def fill_unattributed(commits: list[dict]) -> None:
    """For commits without notes, all insertions are unknown."""
    for c in commits:
        if not c["has_note"]:
            c["unknown_additions"] = c["insertions"]


# ---------------------------------------------------------------------------
# Aggregation
# ---------------------------------------------------------------------------

def aggregate_by_author(commits: list[dict]) -> list[dict]:
    authors: dict[str, dict] = {}
    for c in commits:
        key = c["email"] or c["author"]
        if key not in authors:
            authors[key] = {
                "name": c["author"],
                "email": c["email"],
                "commit_count": 0,
                "total_insertions": 0,
                "total_deletions": 0,
                "ai_additions": 0,
                "human_additions": 0,
                "unknown_additions": 0,
                "mixed_additions": 0,
                "has_note_count": 0,
                "tools": defaultdict(int),
                "dates": [],
            }
        a = authors[key]
        a["name"] = c["author"]
        a["commit_count"] += 1
        a["total_insertions"] += c["insertions"]
        a["total_deletions"] += c["deletions"]
        a["ai_additions"] += c["ai_additions"]
        a["human_additions"] += c["human_additions"]
        a["unknown_additions"] += c["unknown_additions"]
        a["mixed_additions"] += c["mixed_additions"]
        if c["has_note"]:
            a["has_note_count"] += 1
        for tool, cnt in c["tools"].items():
            a["tools"][tool] += cnt
        if c["date"]:
            a["dates"].append(c["date"])

    result = []
    for a in authors.values():
        total = a["ai_additions"] + a["human_additions"] + a["unknown_additions"]
        result.append({
            **a,
            "tools": dict(a["tools"]),
            "total_attributed_lines": total,
            "ai_pct": round(100 * a["ai_additions"] / total, 1) if total > 0 else 0,
            "first_commit": min(a["dates"]) if a["dates"] else "",
            "last_commit": max(a["dates"]) if a["dates"] else "",
        })
    return sorted(result, key=lambda x: x["commit_count"], reverse=True)


def build_timeline(commits: list[dict]) -> dict[str, dict]:
    """Aggregate commits and lines added by date."""
    timeline: dict[str, dict] = {}
    for c in reversed(commits):  # oldest first
        d = c["date"]
        if d not in timeline:
            timeline[d] = {
                "commits": 0,
                "insertions": 0,
                "ai": 0,
                "human": 0,
                "unknown": 0,
            }
        timeline[d]["commits"] += 1
        timeline[d]["insertions"] += c["insertions"]
        timeline[d]["ai"] += c["ai_additions"]
        timeline[d]["human"] += c["human_additions"]
        timeline[d]["unknown"] += c["unknown_additions"]
    return timeline


def collect_all_tools(authors: list[dict]) -> dict[str, int]:
    totals: dict[str, int] = defaultdict(int)
    for a in authors:
        for tool, cnt in a["tools"].items():
            totals[tool] += cnt
    return dict(totals)


# ---------------------------------------------------------------------------
# DB data source (Postgres / Supabase)
# ---------------------------------------------------------------------------

def _since_to_epoch(since: str) -> int:
    """Convert a since string like '30d','7d','1y','YYYY-MM-DD' to a Unix timestamp."""
    import time
    since = since.strip()
    if re.match(r'^\d{4}-\d{2}-\d{2}$', since):
        return int(datetime.strptime(since, "%Y-%m-%d").timestamp())
    m = re.match(r'^(\d+)([dhwmy])$', since)
    if m:
        n, unit = int(m.group(1)), m.group(2)
        secs = {'d': 86400, 'h': 3600, 'w': 604800, 'm': 2592000, 'y': 31536000}[unit]
        return int(time.time()) - n * secs
    raise ValueError(f"Unrecognised --since value: {since!r}")


def collect_from_db(
    database_url: str,
    since: str | None,
    max_count: int | None,
) -> tuple[list[dict], list[dict], dict[str, dict], int]:
    """
    Query the central git_ai_prompts table and return the same
    (commits, authors, timeline, noted_count) tuple that the local
    pipeline produces — so the HTML generator needs no changes.
    """
    try:
        import psycopg2  # type: ignore
        import psycopg2.extras  # type: ignore
    except ImportError:
        print("ERROR: psycopg2 not installed. Run: pip install psycopg2-binary", file=sys.stderr)
        sys.exit(1)

    conn = psycopg2.connect(database_url)
    try:
        with conn.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            since_clause = ""
            params: list = []
            if since and not (hasattr(since, '__class__') and since == 'all'):
                epoch = _since_to_epoch(since)
                since_clause = "WHERE start_time >= %s"
                params.append(epoch)
            limit_clause = ""
            if max_count:
                limit_clause = f"LIMIT {int(max_count)}"
            cur.execute(f"""
                SELECT
                    id, tool, model, human_author, commit_sha,
                    total_additions, total_deletions,
                    accepted_lines, overridden_lines, accepted_rate,
                    start_time, created_at, workdir
                FROM git_ai_prompts
                {since_clause}
                ORDER BY start_time DESC
                {limit_clause}
            """, params)
            rows = cur.fetchall()
    finally:
        conn.close()

    # ---- Build synthetic commit-like dicts grouped by commit_sha ----
    commits_by_sha: dict[str, dict] = {}
    for r in rows:
        sha = r["commit_sha"] or r["id"]  # fall back to prompt id if no sha
        if sha not in commits_by_sha:
            dt = datetime.fromtimestamp(r["start_time"]) if r["start_time"] else datetime.now()
            commits_by_sha[sha] = {
                "sha": sha,
                "author": r["human_author"] or "unknown",
                "email": r["human_author"] or "unknown",
                "date": dt.strftime("%Y-%m-%d"),
                "subject": "",
                "files_changed": 0,
                "insertions": 0,
                "deletions": 0,
                "ai_additions": 0,
                "human_additions": 0,
                "unknown_additions": 0,
                "mixed_additions": 0,
                "has_note": True,
                "tools": defaultdict(int),
            }
        c = commits_by_sha[sha]
        c["insertions"] += r["total_additions"] or 0
        c["deletions"]  += r["total_deletions"] or 0
        c["ai_additions"] += r["accepted_lines"] or 0
        tool_key = r["tool"] or "unknown"
        if r["model"]:
            tool_key = f"{r['tool']}/{r['model']}"
        c["tools"][tool_key] += r["accepted_lines"] or 0

    # Convert tool defaultdicts to plain dicts
    commits = []
    for c in commits_by_sha.values():
        c["tools"] = dict(c["tools"])
        # Lines not attributed to AI are treated as unknown in the DB path
        c["unknown_additions"] = max(0, c["insertions"] - c["ai_additions"])
        commits.append(c)

    authors = aggregate_by_author(commits)
    timeline = build_timeline(commits)
    noted_count = len([c for c in commits if c["has_note"]])
    return commits, authors, timeline, noted_count


# ---------------------------------------------------------------------------
# HTML generation
# ---------------------------------------------------------------------------

_PALETTE = {
    "ai": "#6366f1",
    "human": "#22c55e",
    "unknown": "#94a3b8",
    "mixed": "#f59e0b",
    "commits": "#3b82f6",
    "insertions": "#10b981",
    "deletions": "#ef4444",
}


def _esc(s: object) -> str:
    return html.escape(str(s))


def _js(obj: object) -> str:
    return json.dumps(obj)


def generate_html(
    commits: list[dict],
    authors: list[dict],
    timeline: dict[str, dict],
    noted_count: int,
    args_desc: str,
) -> str:
    total_commits = len(commits)
    total_authors = len(authors)
    total_ai = sum(a["ai_additions"] for a in authors)
    total_human = sum(a["human_additions"] for a in authors)
    total_unknown = sum(a["unknown_additions"] for a in authors)
    total_lines = total_ai + total_human + total_unknown
    ai_pct = round(100 * total_ai / total_lines, 1) if total_lines > 0 else 0
    all_tools = collect_all_tools(authors)
    generated_at = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    data_source_label = "central DB (Postgres)" if args_desc.startswith("DB:") else "local repo only \u2014 no data sent externally"

    # ---- Timeline datasets ----
    tl_dates = list(timeline.keys())
    tl_commits = [timeline[d]["commits"] for d in tl_dates]
    tl_ai = [timeline[d]["ai"] for d in tl_dates]
    tl_human = [timeline[d]["human"] for d in tl_dates]
    tl_unknown = [timeline[d]["unknown"] for d in tl_dates]

    # ---- Per-author bar (top 20) ----
    top_authors = authors[:20]
    bar_labels = [_esc(a["name"]) for a in top_authors]
    bar_ai = [a["ai_additions"] for a in top_authors]
    bar_human = [a["human_additions"] for a in top_authors]
    bar_unknown = [a["unknown_additions"] for a in top_authors]
    bar_commits = [a["commit_count"] for a in top_authors]

    # ---- Author table rows ----
    rows_html = ""
    for a in authors:
        tl = a["total_attributed_lines"]
        ai_bar = round(100 * a["ai_additions"] / tl) if tl > 0 else 0
        hm_bar = round(100 * a["human_additions"] / tl) if tl > 0 else 0
        uk_bar = 100 - ai_bar - hm_bar
        tools_str = ", ".join(f"{t}:{c}" for t, c in sorted(a["tools"].items())) or "—"
        rows_html += f"""
        <tr>
          <td title="{_esc(a['email'])}">{_esc(a['name'])}</td>
          <td class="num">{a['commit_count']}</td>
          <td class="num">{a['total_insertions']:,}</td>
          <td class="num">{a['total_deletions']:,}</td>
          <td>
            <div class="attr-bar">
              <div class="seg ai"  style="width:{ai_bar}%" title="AI: {a['ai_additions']:,}"></div>
              <div class="seg hm"  style="width:{hm_bar}%" title="Human: {a['human_additions']:,}"></div>
              <div class="seg uk"  style="width:{uk_bar}%" title="Unknown: {a['unknown_additions']:,}"></div>
            </div>
          </td>
          <td class="num ai-pct">{a['ai_pct']}%</td>
          <td class="num">{a['ai_additions']:,}</td>
          <td class="num">{a['human_additions']:,}</td>
          <td class="num">{a['unknown_additions']:,}</td>
          <td class="tools">{_esc(tools_str)}</td>
          <td class="date">{a['first_commit']}</td>
          <td class="date">{a['last_commit']}</td>
        </tr>"""

    tools_rows_html = ""
    for tool, cnt in sorted(all_tools.items(), key=lambda x: -x[1]):
        tools_rows_html += f"<tr><td>{_esc(tool)}</td><td class='num'>{cnt:,}</td></tr>"
    if not tools_rows_html:
        tools_rows_html = "<tr><td colspan='2' style='color:#94a3b8'>No AI tool data yet — commit with an AI agent to see breakdown</td></tr>"

    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>git-ai Dashboard</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.3/dist/chart.umd.min.js"></script>
<style>
  :root {{
    --bg: #0f172a; --surface: #1e293b; --surface2: #2d3f55;
    --border: #334155; --text: #e2e8f0; --muted: #94a3b8;
    --ai: #6366f1; --human: #22c55e; --unknown: #94a3b8;
    --accent: #38bdf8;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ background: var(--bg); color: var(--text); font-family: system-ui, -apple-system, sans-serif; padding: 1.5rem; }}
  h1 {{ font-size: 1.5rem; font-weight: 700; margin-bottom: .25rem; }}
  .meta {{ color: var(--muted); font-size: .85rem; margin-bottom: 1.5rem; }}
  .meta a {{ color: var(--accent); text-decoration: none; }}
  /* Cards */
  .cards {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: .75rem; margin-bottom: 1.5rem; }}
  .card {{ background: var(--surface); border: 1px solid var(--border); border-radius: .75rem; padding: 1rem; }}
  .card .label {{ font-size: .75rem; color: var(--muted); margin-bottom: .25rem; text-transform: uppercase; letter-spacing: .05em; }}
  .card .value {{ font-size: 1.6rem; font-weight: 700; }}
  .card .value.ai {{ color: var(--ai); }}
  .card .value.human {{ color: var(--human); }}
  .card .value.unknown {{ color: var(--unknown); }}
  /* Charts */
  .charts {{ display: grid; grid-template-columns: 1fr 2fr; gap: 1rem; margin-bottom: 1.5rem; }}
  @media (max-width: 800px) {{ .charts {{ grid-template-columns: 1fr; }} }}
  .chart-box {{ background: var(--surface); border: 1px solid var(--border); border-radius: .75rem; padding: 1rem; }}
  .chart-box h2 {{ font-size: .9rem; font-weight: 600; color: var(--muted); margin-bottom: .75rem; text-transform: uppercase; letter-spacing: .05em; }}
  .chart-full {{ background: var(--surface); border: 1px solid var(--border); border-radius: .75rem; padding: 1rem; margin-bottom: 1rem; }}
  .chart-full h2 {{ font-size: .9rem; font-weight: 600; color: var(--muted); margin-bottom: .75rem; text-transform: uppercase; letter-spacing: .05em; }}
  canvas {{ max-height: 320px; }}
  /* Table */
  .table-wrap {{ overflow-x: auto; background: var(--surface); border: 1px solid var(--border); border-radius: .75rem; margin-bottom: 1rem; }}
  table {{ border-collapse: collapse; width: 100%; font-size: .83rem; }}
  th {{ background: var(--surface2); color: var(--muted); padding: .6rem .8rem; text-align: left; font-weight: 600; text-transform: uppercase; font-size: .72rem; letter-spacing: .05em; white-space: nowrap; cursor: pointer; user-select: none; }}
  th:hover {{ color: var(--text); }}
  td {{ padding: .55rem .8rem; border-top: 1px solid var(--border); vertical-align: middle; }}
  tr:hover td {{ background: var(--surface2); }}
  .num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  .date {{ font-size: .78rem; color: var(--muted); white-space: nowrap; }}
  .tools {{ font-size: .75rem; color: var(--muted); max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
  .ai-pct {{ color: var(--ai); font-weight: 600; }}
  /* Attribution bar */
  .attr-bar {{ display: flex; height: 8px; border-radius: 4px; overflow: hidden; background: var(--border); min-width: 80px; }}
  .seg {{ height: 100%; }}
  .seg.ai {{ background: var(--ai); }}
  .seg.hm {{ background: var(--human); }}
  .seg.uk {{ background: var(--unknown); }}
  /* Legend */
  .legend {{ display: flex; gap: 1rem; font-size: .8rem; color: var(--muted); margin-bottom: .5rem; flex-wrap: wrap; }}
  .legend span {{ display: flex; align-items: center; gap: .3rem; }}
  .dot {{ width: 10px; height: 10px; border-radius: 50%; display: inline-block; }}
  /* Tools table */
  .two-col {{ display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-bottom: 1rem; }}
  @media (max-width: 700px) {{ .two-col {{ grid-template-columns: 1fr; }} }}
  .section-title {{ font-size: 1rem; font-weight: 700; margin: 1.25rem 0 .5rem; }}
  .badge {{ display: inline-block; background: var(--surface2); border: 1px solid var(--border); border-radius: 999px; padding: .1rem .55rem; font-size: .72rem; color: var(--muted); margin-left: .4rem; }}
  .note-banner {{ background: #1e3a2f; border: 1px solid #166534; border-radius: .5rem; padding: .75rem 1rem; font-size: .83rem; color: #86efac; margin-bottom: 1.25rem; }}
  .warn-banner {{ background: #2d1f05; border: 1px solid #92400e; border-radius: .5rem; padding: .75rem 1rem; font-size: .83rem; color: #fcd34d; margin-bottom: 1.25rem; }}
  input[type=search] {{ background: var(--surface2); border: 1px solid var(--border); color: var(--text); border-radius: .4rem; padding: .35rem .75rem; font-size: .85rem; outline: none; width: 220px; }}
  input[type=search]::placeholder {{ color: var(--muted); }}
  .table-controls {{ display: flex; justify-content: space-between; align-items: center; padding: .5rem .8rem; background: var(--surface2); border-radius: .75rem .75rem 0 0; border: 1px solid var(--border); border-bottom: none; font-size: .82rem; color: var(--muted); }}
  tfoot td {{ border-top: 2px solid var(--border); color: var(--muted); font-weight: 600; }}
</style>
</head>
<body>

<h1>git-ai Attribution Dashboard</h1>
<p class="meta">
  Generated: {generated_at} &nbsp;·&nbsp; Scope: <code>{_esc(args_desc)}</code>
  &nbsp;·&nbsp; Data source: {_esc(data_source_label)}.
  &nbsp;·&nbsp; <a href="https://usegitai.com" target="_blank" rel="noopener">usegitai.com</a>
</p>

{'<div class="warn-banner">⚠ No git-ai attribution notes found in this clone. AI % shows 0 for all authors. Run <code>git-ai fetch-notes</code> or commit code via an AI agent with git-ai hooks installed to populate attribution data.</div>' if noted_count == 0 else f'<div class="note-banner">✓ {noted_count} commit(s) have git-ai attribution notes.</div>'}

<!-- Summary cards -->
<div class="cards">
  <div class="card"><div class="label">Commits</div><div class="value">{total_commits:,}</div></div>
  <div class="card"><div class="label">Authors</div><div class="value">{total_authors:,}</div></div>
  <div class="card"><div class="label">Lines Added</div><div class="value">{sum(c['insertions'] for c in commits):,}</div></div>
  <div class="card"><div class="label">AI Lines</div><div class="value ai">{total_ai:,}</div></div>
  <div class="card"><div class="label">Human Lines</div><div class="value human">{total_human:,}</div></div>
  <div class="card"><div class="label">Unattributed</div><div class="value unknown">{total_unknown:,}</div></div>
  <div class="card"><div class="label">AI %</div><div class="value ai">{ai_pct}%</div></div>
  <div class="card"><div class="label">Attributed Commits</div><div class="value">{noted_count:,}</div></div>
</div>

<!-- Attribution donut + per-author bar -->
<div class="charts">
  <div class="chart-box">
    <h2>Overall Attribution</h2>
    <canvas id="donutChart"></canvas>
  </div>
  <div class="chart-box">
    <h2>Top 20 Contributors — Line Attribution</h2>
    <canvas id="authorBar"></canvas>
  </div>
</div>

<!-- Timeline -->
<div class="chart-full">
  <h2>Daily Commits Timeline</h2>
  <canvas id="timelineChart"></canvas>
</div>

<!-- AI Tools -->
<div class="section-title">AI Tool Breakdown</div>
<div class="two-col">
  <div class="table-wrap">
    <table>
      <thead><tr><th>Tool / Model</th><th>Line Count</th></tr></thead>
      <tbody>{tools_rows_html}</tbody>
    </table>
  </div>
  <div class="chart-box" style="align-self:start">
    <h2>AI Tools Distribution</h2>
    <canvas id="toolsChart"></canvas>
  </div>
</div>

<!-- Authors table -->
<div class="section-title">All Authors <span class="badge">{total_authors}</span></div>
<div class="legend">
  <span><span class="dot" style="background:var(--ai)"></span>AI</span>
  <span><span class="dot" style="background:var(--human)"></span>Known Human</span>
  <span><span class="dot" style="background:var(--unknown)"></span>Unattributed</span>
</div>
<div class="table-controls">
  <span>Click column headers to sort</span>
  <input type="search" id="authorSearch" placeholder="Filter authors…" oninput="filterTable(this.value)">
</div>
<div class="table-wrap" style="border-radius:0 0 .75rem .75rem; border-top: none;">
  <table id="authorTable">
    <thead>
      <tr>
        <th onclick="sortTable(0)">Author</th>
        <th onclick="sortTable(1)" class="num">Commits</th>
        <th onclick="sortTable(2)" class="num">Lines+</th>
        <th onclick="sortTable(3)" class="num">Lines-</th>
        <th>Attribution</th>
        <th onclick="sortTable(5)" class="num">AI%</th>
        <th onclick="sortTable(6)" class="num">AI Lines</th>
        <th onclick="sortTable(7)" class="num">Human Lines</th>
        <th onclick="sortTable(8)" class="num">Unattr. Lines</th>
        <th>AI Tools</th>
        <th onclick="sortTable(10)">First Commit</th>
        <th onclick="sortTable(11)">Last Commit</th>
      </tr>
    </thead>
    <tbody>{rows_html}</tbody>
    <tfoot>
      <tr>
        <td>TOTAL</td>
        <td class="num">{total_commits:,}</td>
        <td class="num">{sum(c['insertions'] for c in commits):,}</td>
        <td class="num">{sum(c['deletions'] for c in commits):,}</td>
        <td></td>
        <td class="num ai-pct">{ai_pct}%</td>
        <td class="num">{total_ai:,}</td>
        <td class="num">{total_human:,}</td>
        <td class="num">{total_unknown:,}</td>
        <td></td><td></td><td></td>
      </tr>
    </tfoot>
  </table>
</div>

<script>
// ---- Chart defaults ----
Chart.defaults.color = '#94a3b8';
Chart.defaults.borderColor = '#334155';
Chart.defaults.font.family = "system-ui, -apple-system, sans-serif";

// ---- Donut: overall attribution ----
new Chart(document.getElementById('donutChart'), {{
  type: 'doughnut',
  data: {{
    labels: ['AI', 'Known Human', 'Unattributed'],
    datasets: [{{
      data: [{total_ai}, {total_human}, {total_unknown}],
      backgroundColor: ['#6366f1', '#22c55e', '#94a3b8'],
      borderColor: '#1e293b',
      borderWidth: 2,
    }}],
  }},
  options: {{
    responsive: true,
    plugins: {{
      legend: {{ position: 'bottom', labels: {{ padding: 16, boxWidth: 12 }} }},
      tooltip: {{
        callbacks: {{
          label: ctx => {{
            const total = ctx.dataset.data.reduce((a,b) => a+b, 0);
            const pct = total > 0 ? (ctx.parsed / total * 100).toFixed(1) : 0;
            return ` ${{ctx.label}}: ${{ctx.parsed.toLocaleString()}} lines (${{pct}}%)`;
          }}
        }}
      }}
    }},
  }},
}});

// ---- Stacked bar: top authors ----
new Chart(document.getElementById('authorBar'), {{
  type: 'bar',
  data: {{
    labels: {_js(bar_labels)},
    datasets: [
      {{ label: 'AI',          data: {_js(bar_ai)},      backgroundColor: '#6366f1' }},
      {{ label: 'Human',       data: {_js(bar_human)},   backgroundColor: '#22c55e' }},
      {{ label: 'Unattributed',data: {_js(bar_unknown)}, backgroundColor: '#94a3b8' }},
    ],
  }},
  options: {{
    indexAxis: 'y',
    responsive: true,
    scales: {{
      x: {{ stacked: true, ticks: {{ callback: v => v.toLocaleString() }} }},
      y: {{ stacked: true, ticks: {{ font: {{ size: 11 }} }} }},
    }},
    plugins: {{
      legend: {{ position: 'top', labels: {{ boxWidth: 12 }} }},
    }},
  }},
}});

// ---- Line chart: timeline ----
new Chart(document.getElementById('timelineChart'), {{
  type: 'bar',
  data: {{
    labels: {_js(tl_dates)},
    datasets: [
      {{ label: 'Commits',     data: {_js(tl_commits)},  backgroundColor: '#3b82f6',  yAxisID: 'commits' }},
      {{ label: 'AI Lines',    data: {_js(tl_ai)},       backgroundColor: '#6366f150', borderColor: '#6366f1', borderWidth: 1, yAxisID: 'lines', type: 'line', fill: true, pointRadius: 0 }},
      {{ label: 'Human Lines', data: {_js(tl_human)},    backgroundColor: '#22c55e50', borderColor: '#22c55e', borderWidth: 1, yAxisID: 'lines', type: 'line', fill: true, pointRadius: 0 }},
      {{ label: 'Unattr. Lines',data: {_js(tl_unknown)}, backgroundColor: '#94a3b820', borderColor: '#94a3b8', borderWidth: 1, yAxisID: 'lines', type: 'line', fill: true, pointRadius: 0 }},
    ],
  }},
  options: {{
    responsive: true,
    interaction: {{ mode: 'index', intersect: false }},
    scales: {{
      commits: {{ type: 'linear', position: 'left',  title: {{ display: true, text: 'Commits' }} }},
      lines:   {{ type: 'linear', position: 'right', title: {{ display: true, text: 'Lines' }}, grid: {{ drawOnChartArea: false }} }},
    }},
    plugins: {{ legend: {{ position: 'top', labels: {{ boxWidth: 12 }} }} }},
  }},
}});

// ---- Tools pie ----
(function() {{
  const toolLabels = {_js(list(all_tools.keys()))};
  const toolData   = {_js(list(all_tools.values()))};
  if (toolLabels.length === 0) {{
    document.getElementById('toolsChart').parentElement.innerHTML += '<p style="color:#94a3b8;font-size:.83rem;margin-top:.5rem">No AI tool data yet.</p>';
    return;
  }}
  new Chart(document.getElementById('toolsChart'), {{
    type: 'pie',
    data: {{
      labels: toolLabels,
      datasets: [{{ data: toolData, backgroundColor: ['#6366f1','#22c55e','#f59e0b','#38bdf8','#a855f7','#ec4899','#10b981','#fb923c'], borderColor: '#1e293b', borderWidth: 2 }}],
    }},
    options: {{ responsive: true, plugins: {{ legend: {{ position: 'bottom', labels: {{ boxWidth: 12 }} }} }} }},
  }});
}})();

// ---- Table sort ----
let _sortCol = 1, _sortAsc = false;
function sortTable(col) {{
  const t = document.getElementById('authorTable');
  const tbody = t.tBodies[0];
  const rows = Array.from(tbody.rows);
  if (_sortCol === col) _sortAsc = !_sortAsc; else {{ _sortCol = col; _sortAsc = col === 0; }}
  rows.sort((a, b) => {{
    let av = a.cells[col].innerText.trim().replace(/[%,]/g,'');
    let bv = b.cells[col].innerText.trim().replace(/[%,]/g,'');
    const an = parseFloat(av), bn = parseFloat(bv);
    const cmp = isNaN(an) ? av.localeCompare(bv) : an - bn;
    return _sortAsc ? cmp : -cmp;
  }});
  rows.forEach(r => tbody.appendChild(r));
}}

// ---- Table filter ----
function filterTable(q) {{
  q = q.toLowerCase();
  const rows = document.querySelectorAll('#authorTable tbody tr');
  rows.forEach(r => {{
    const text = r.cells[0].innerText.toLowerCase() + r.cells[0].title.toLowerCase();
    r.style.display = text.includes(q) ? '' : 'none';
  }});
}}
</script>
</body>
</html>"""


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Generate a git-ai attribution dashboard (HTML).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--since", default="90d", help="Limit commits/prompts by age (default: 90d)")
    parser.add_argument("--all", dest="all_commits", action="store_true", help="Include all history")
    parser.add_argument("--max-count", type=int, default=None, help="Max commits to process")
    parser.add_argument("--output", default="git-ai-dashboard.html", help="Output HTML file (default: git-ai-dashboard.html)")
    parser.add_argument("--no-enrich", action="store_true", help="Skip git-ai stats lookup (local mode faster)")
    parser.add_argument("--db", action="store_true", help="Read from central Postgres/Supabase DB")
    parser.add_argument("--db-url", default=None, help="Postgres connection URL (overrides DATABASE_URL env)")
    args = parser.parse_args()

    since = None if args.all_commits else args.since

    print("git-ai dashboard generator", file=sys.stderr)

    if args.db or args.db_url:
        # ---- Central DB mode ----
        database_url = args.db_url or os.environ.get("DATABASE_URL", "").strip()
        if not database_url:
            print("ERROR: --db requires either --db-url or DATABASE_URL env var.", file=sys.stderr)
            sys.exit(1)
        scope = "all time" if args.all_commits else f"since {since}"
        args_desc = f"DB: {scope}" + (f", max {args.max_count}" if args.max_count else "")
        print(f"  Mode: central DB", file=sys.stderr)
        print(f"  Scope: {args_desc}", file=sys.stderr)
        commits, authors, timeline, noted_count = collect_from_db(
            database_url, since if not args.all_commits else None, args.max_count
        )
        print(f"  {len(commits)} commit-equivalents, {noted_count} attributed", file=sys.stderr)
    else:
        # ---- Local git mode ----
        args_desc = "all commits" if args.all_commits else f"since {since}" + (f", max {args.max_count}" if args.max_count else "")
        print(f"  Mode: local git log", file=sys.stderr)
        print(f"  Scope: {args_desc}", file=sys.stderr)
        print("  Collecting commits from git log ...", file=sys.stderr)
        commits = collect_commits(max_count=args.max_count, since=since)
        print(f"  Found {len(commits)} commits", file=sys.stderr)
        noted: set[str] = set()
        if not args.no_enrich:
            print("  Checking for git-ai attribution notes ...", file=sys.stderr)
            noted = get_noted_commits()
            print(f"  {len(noted)} commit(s) have notes", file=sys.stderr)
            enrich_with_git_ai(commits, noted)
        fill_unattributed(commits)
        print("  Aggregating by author ...", file=sys.stderr)
        authors = aggregate_by_author(commits)
        timeline = build_timeline(commits)
        noted_count = len(noted)

    print(f"  Generating HTML -> {args.output} ...", file=sys.stderr)
    html_out = generate_html(commits, authors, timeline, noted_count, args_desc)

    out_path = Path(args.output)
    out_path.write_text(html_out, encoding="utf-8")
    print(f"  Done! Open: {out_path.resolve()}", file=sys.stderr)


if __name__ == "__main__":
    main()
