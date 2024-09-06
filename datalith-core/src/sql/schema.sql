-- File Table
CREATE TABLE `files` (
    -- UUID (128-bit)
    `id`          BLOB    NOT NULL PRIMARY KEY,
    -- hashed by SHA-256
    `hash`        BLOB    NOT NULL UNIQUE,
    -- UNIX timestamp (in milliseconds)
    `created_at`  INTEGER NOT NULL,
    -- in bytes
    `file_size`   INTEGER NOT NULL,
    -- MIME type
    `file_type`   TEXT    NOT NULL,
    -- the file name when it was first created
    `file_name`   TEXT    NOT NULL,
    -- the number of times this file was created
    `count`       INTEGER NOT NULL DEFAULT 1,
    -- UNIX timestamp (in milliseconds). If this exists, the file is temporary
    `expired_at`  INTEGER
);

CREATE INDEX `files_created_at` ON `files` (`created_at`);
CREATE INDEX `files_expired_at` ON `files` (`expired_at`);

-- Resource Table
CREATE TABLE `resources` (
    -- UUID (128-bit)
    `id`           BLOB    NOT NULL PRIMARY KEY,
    -- UNIX timestamp (in milliseconds)
    `created_at`   INTEGER NOT NULL,
    -- MIME type
    `file_type`    TEXT    NOT NULL,
    -- the file name when it was first created
    `file_name`    TEXT    NOT NULL,
    -- UUID (128-bit)
    `file_id`      BLOB    NOT NULL,
    -- UNIX timestamp (in milliseconds). If this exists, the resource is temporary
    `expired_at`  INTEGER,

    FOREIGN KEY (`file_id`) REFERENCES `files` (`id`)
);

CREATE INDEX `resources_created_at` ON `resources` (`created_at`);

-- Image Table
CREATE TABLE `images` (
    -- UUID (128-bit)
    `id`                 BLOB    NOT NULL PRIMARY KEY,
    -- UNIX timestamp (in milliseconds)
    `created_at`         INTEGER NOT NULL,
    -- the file stem of this image
    `image_stem`         TEXT    NOT NULL,
    -- the width of 1x image (in pixels)
    `image_width`        INTEGER NOT NULL,
    -- the height of 1x image (in pixels)
    `image_height`       INTEGER NOT NULL,
    -- UUID (128-bit)
    `original_file_id`   BLOB,
    -- boolean
    `has_alpha_channel`  INTEGER NOT NULL,

    FOREIGN KEY (`original_file_id`) REFERENCES `files` (`id`)
);

CREATE INDEX `images_created_at` ON `images` (`created_at`);

-- Image Thumbnail Table
CREATE TABLE `image_thumbnails` (
    -- UUID (128-bit)
    `image_id`     BLOB    NOT NULL,
    `multiplier`   INTEGER NOT NULL,
    -- boolean
    `fallback`     INTEGER NOT NULL,
    -- UUID (128-bit)
    `file_id`      BLOB    NOT NULL,

    PRIMARY KEY (`image_id`, `multiplier`, `fallback`),
    FOREIGN KEY (`image_id`) REFERENCES `images` (`id`),
    FOREIGN KEY (`file_id`) REFERENCES `files` (`id`)
);