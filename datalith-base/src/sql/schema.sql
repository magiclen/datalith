CREATE TABLE files (
    id          BLOB    NOT NULL PRIMARY KEY,
    hash        BLOB    NOT NULL UNIQUE,
    create_time INTEGER NOT NULL,
    file_size   INTEGER NOT NULL,
    file_type   TEXT    NOT NULL,
    file_name   TEXT    NOT NULL,
    count       INTEGER NOT NULL,
    expired_at  INTEGER
);

CREATE INDEX files_create_time ON files (create_time);

CREATE TABLE files_locks (
    hash        BLOB    NOT NULL PRIMARY KEY,
    create_time INTEGER NOT NULL
);

CREATE INDEX files_locks_create_time ON files (create_time);