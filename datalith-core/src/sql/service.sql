-- Media, task, and playback tables for version 2.
CREATE TABLE IF NOT EXISTS blob_files (
    file_id BLOB PRIMARY KEY NOT NULL,
    hash BLOB NOT NULL,
    storage_id BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS blob_files_hash ON blob_files(hash);
CREATE INDEX IF NOT EXISTS blob_files_storage ON blob_files(storage_id);
CREATE TABLE IF NOT EXISTS media (
    id BLOB PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    single_use INTEGER NOT NULL,
    consumed_at INTEGER,
    metadata TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS media_created_at ON media(created_at, id);
CREATE INDEX IF NOT EXISTS media_expiry ON media(expires_at);
CREATE INDEX IF NOT EXISTS media_consumed ON media(consumed_at) WHERE consumed_at IS NOT NULL;
CREATE TABLE IF NOT EXISTS media_files (
    media_id BLOB NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    file_id BLOB NOT NULL REFERENCES files(id),
    PRIMARY KEY(media_id, role)
);
CREATE INDEX IF NOT EXISTS media_files_file ON media_files(file_id);
CREATE TABLE IF NOT EXISTS tasks (
    id BLOB PRIMARY KEY NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    metadata TEXT NOT NULL,
    work TEXT NOT NULL,
    idempotency_key TEXT UNIQUE,
    fingerprint TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tasks_queue ON tasks(status, created_at, id);
CREATE TABLE IF NOT EXISTS archive_imports (
    archive_id BLOB PRIMARY KEY NOT NULL,
    digest TEXT NOT NULL,
    result TEXT NOT NULL
);

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
