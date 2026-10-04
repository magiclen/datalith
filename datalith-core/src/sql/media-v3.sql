-- The HLS inventory is separate from the public media summary.
CREATE TABLE IF NOT EXISTS media_hls (
    media_id BLOB PRIMARY KEY NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    inventory TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS playback_sessions (
    media_id BLOB PRIMARY KEY NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    token_hash TEXT UNIQUE NOT NULL,
    expires_at INTEGER NOT NULL,
    idempotency_key TEXT UNIQUE
);
CREATE INDEX IF NOT EXISTS playback_sessions_expiry ON playback_sessions(expires_at);
CREATE TABLE IF NOT EXISTS mp4_artifacts (
    task_id BLOB PRIMARY KEY NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    media_id BLOB NOT NULL,
    session_hash TEXT,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS mp4_artifacts_expiry ON mp4_artifacts(expires_at);
