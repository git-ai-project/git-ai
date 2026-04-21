-- git-ai dashboard schema
-- Run with: psql "$DATABASE_URL" -f scripts/db-schema.sql

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
