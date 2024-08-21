-- File Table
CREATE TABLE files (
    -- UUID (128-bit)
    id          BLOB    NOT NULL PRIMARY KEY,
    -- hashed by SHA-256
    hash        BLOB    NOT NULL UNIQUE,
    -- UNIX timestamp (in milliseconds)
    created_at  INTEGER NOT NULL,
    -- in bytes
    file_size   INTEGER NOT NULL,
    -- MIME type
    file_type   TEXT    NOT NULL,
    -- the file name when it was first created
    file_name   TEXT    NOT NULL,
    -- the number of times this file was created
    count       INTEGER NOT NULL DEFAULT 1,
    -- UNIX timestamp (in milliseconds). If this exists, the file is temporary
    expired_at  INTEGER
);

CREATE INDEX files_created_at ON files (created_at);
CREATE INDEX files_expired_at ON files (expired_at);