-- The HLS inventory is separate from the public media summary.
CREATE TABLE IF NOT EXISTS media_hls (
    media_id BLOB PRIMARY KEY NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    inventory TEXT NOT NULL
);
