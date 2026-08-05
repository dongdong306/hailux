-- 初始 schema（0.4.0 起由版本化迁移接管）。
-- 使用 IF NOT EXISTS 保持幂等，兼容旧版手写迁移遗留的库。
CREATE TABLE IF NOT EXISTS sessions (
    id                TEXT PRIMARY KEY,
    title             TEXT NOT NULL DEFAULT '',
    model             TEXT NOT NULL DEFAULT '',
    work_dir          TEXT NOT NULL DEFAULT '',
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    parent_id         TEXT,
    compact_summary   TEXT
);

CREATE TABLE IF NOT EXISTS messages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role              TEXT NOT NULL,
    content           TEXT NOT NULL DEFAULT '',
    tool_calls        TEXT,
    tool_call_id      TEXT,
    reasoning_content TEXT,
    prompt_tokens     INTEGER,
    completion_tokens INTEGER,
    runtime_meta      TEXT,
    think_ms          INTEGER,
    compacted         INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
