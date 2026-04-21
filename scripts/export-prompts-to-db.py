#!/usr/bin/env python3
"""
export-prompts-to-db.py
=======================
Reads the local prompts.db (SQLite) produced by `git-ai prompts`
and upserts all rows into a central Postgres / Supabase database.

Environment variables (required):
  DATABASE_URL  — postgres connection string
                  postgresql://user:pass@host:5432/dbname
                  or a Supabase transaction-pooler URL

Optional:
  PROMPTS_DB    — path to prompts.db  (default: prompts.db)

The script is idempotent: re-running it upserts on the prompt `id`
column so no duplicates are created.

Schema created automatically if it does not exist:

   CREATE TABLE IF NOT EXISTS git_ai_prompts (
       id                TEXT PRIMARY KEY,
       tool              TEXT,
       model             TEXT,
       external_thread_id TEXT,
       human_author      TEXT,
       commit_sha        TEXT,
       workdir           TEXT,
       total_additions   INTEGER,
       total_deletions   INTEGER,
       accepted_lines    INTEGER,
       overridden_lines  INTEGER,
       accepted_rate     REAL,
       start_time        BIGINT,
       last_time         BIGINT,
       created_at        BIGINT,
       updated_at        BIGINT,
       messages          TEXT
   );
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
from pathlib import Path


def get_env(name: str) -> str:
    val = os.environ.get(name, "").strip()
    if not val:
        print(f"ERROR: environment variable {name} is required but not set.", file=sys.stderr)
        sys.exit(1)
    return val


DDL = """
CREATE TABLE IF NOT EXISTS git_ai_prompts (
    id                TEXT PRIMARY KEY,
    tool              TEXT,
    model             TEXT,
    external_thread_id TEXT,
    human_author      TEXT,
    commit_sha        TEXT,
    workdir           TEXT,
    total_additions   INTEGER,
    total_deletions   INTEGER,
    accepted_lines    INTEGER,
    overridden_lines  INTEGER,
    accepted_rate     REAL,
    start_time        BIGINT,
    last_time         BIGINT,
    created_at        BIGINT,
    updated_at        BIGINT,
    messages          TEXT
);
CREATE INDEX IF NOT EXISTS idx_gap_tool         ON git_ai_prompts(tool);
CREATE INDEX IF NOT EXISTS idx_gap_human_author ON git_ai_prompts(human_author);
CREATE INDEX IF NOT EXISTS idx_gap_start_time   ON git_ai_prompts(start_time);
CREATE INDEX IF NOT EXISTS idx_gap_commit_sha   ON git_ai_prompts(commit_sha);
"""

UPSERT = """
INSERT INTO git_ai_prompts (
    id, tool, model, external_thread_id,
    human_author, commit_sha, workdir,
    total_additions, total_deletions,
    accepted_lines, overridden_lines, accepted_rate,
    start_time, last_time, created_at, updated_at, messages
) VALUES (
    %s, %s, %s, %s,
    %s, %s, %s,
    %s, %s,
    %s, %s, %s,
    %s, %s, %s, %s, %s
)
ON CONFLICT (id) DO UPDATE SET
    tool              = EXCLUDED.tool,
    model             = EXCLUDED.model,
    human_author      = EXCLUDED.human_author,
    commit_sha        = EXCLUDED.commit_sha,
    total_additions   = EXCLUDED.total_additions,
    total_deletions   = EXCLUDED.total_deletions,
    accepted_lines    = EXCLUDED.accepted_lines,
    overridden_lines  = EXCLUDED.overridden_lines,
    accepted_rate     = EXCLUDED.accepted_rate,
    start_time        = EXCLUDED.start_time,
    last_time         = EXCLUDED.last_time,
    updated_at        = EXCLUDED.updated_at,
    messages          = EXCLUDED.messages;
"""


def read_sqlite(db_path: str) -> list[tuple]:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute("""
            SELECT
                id, tool, model, external_thread_id,
                human_author, commit_sha, workdir,
                total_additions, total_deletions,
                accepted_lines, overridden_lines, accepted_rate,
                start_time, last_time, created_at, updated_at, messages
            FROM prompts
        """).fetchall()
        return [tuple(r) for r in rows]
    finally:
        conn.close()


def export_to_postgres(rows: list[tuple], database_url: str) -> None:
    import psycopg2  # type: ignore

    print(f"  Connecting to Postgres ...", file=sys.stderr)
    conn = psycopg2.connect(database_url)
    try:
        with conn:
            with conn.cursor() as cur:
                for stmt in DDL.strip().split(";"):
                    stmt = stmt.strip()
                    if stmt:
                        cur.execute(stmt)
                print(f"  Upserting {len(rows)} prompts ...", file=sys.stderr)
                batch_size = 200
                inserted = 0
                for i in range(0, len(rows), batch_size):
                    chunk = rows[i : i + batch_size]
                    cur.executemany(UPSERT, chunk)
                    inserted += len(chunk)
                    print(f"    {inserted}/{len(rows)} ...", file=sys.stderr)
        print(f"  Done. {len(rows)} rows upserted.", file=sys.stderr)
    finally:
        conn.close()


def main() -> None:
    db_path = os.environ.get("PROMPTS_DB", "prompts.db")
    database_url = get_env("DATABASE_URL")

    if not Path(db_path).exists():
        print(f"ERROR: {db_path} not found. Run `git-ai prompts --all-authors` first.", file=sys.stderr)
        sys.exit(1)

    print(f"export-prompts-to-db", file=sys.stderr)
    print(f"  Source: {db_path}", file=sys.stderr)
    print(f"  Target: Postgres (from DATABASE_URL)", file=sys.stderr)

    rows = read_sqlite(db_path)
    print(f"  Read {len(rows)} prompts from SQLite", file=sys.stderr)

    if not rows:
        print("  Nothing to export.", file=sys.stderr)
        return

    export_to_postgres(rows, database_url)


if __name__ == "__main__":
    main()
