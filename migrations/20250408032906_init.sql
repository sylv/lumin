CREATE TABLE torrents (
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    hash TEXT NOT NULL CHECK (length(hash) = 40 AND lower(hash) = hash),
    name TEXT NOT NULL,
    remote_id INTEGER UNIQUE,
    magnet_uri TEXT NOT NULL,
    state INTEGER NOT NULL DEFAULT 0,
    progress REAL NOT NULL DEFAULT 0.0,
    upload_speed INTEGER NOT NULL DEFAULT 0,
    download_speed INTEGER NOT NULL DEFAULT 0,
    ratio REAL NOT NULL DEFAULT 0.0,
    eta_secs INTEGER NOT NULL DEFAULT 0,
    seeds INTEGER NOT NULL DEFAULT 0,
    peers INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    save_dir TEXT,
    category TEXT,
    files_created INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    checked_at INTEGER,
    finished_at INTEGER
) STRICT;

CREATE TABLE torrent_blocks (
    hash TEXT NOT NULL PRIMARY KEY CHECK (length(hash) = 40 AND lower(hash) = hash),
    block_reason TEXT NOT NULL,
    blocked_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE TABLE torrent_files (
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    remote_id INTEGER NOT NULL,
    torrent_id INTEGER NOT NULL,
    duration_hint_secs INTEGER,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,

    FOREIGN KEY (torrent_id) REFERENCES torrents(id) ON DELETE CASCADE,
    UNIQUE (torrent_id, path),
    UNIQUE(torrent_id, remote_id)
) STRICT;

CREATE TABLE nodes (
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER,
    name TEXT NOT NULL,
    size INTEGER NOT NULL DEFAULT 0,
    torrent_id INTEGER,
    file_id INTEGER,
    is_automatic INTEGER NOT NULL, -- mostly for files added to the downloads dir automatically
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),

    FOREIGN KEY (parent_id) REFERENCES nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (torrent_id) REFERENCES torrents(id) ON DELETE CASCADE,
    FOREIGN KEY (file_id) REFERENCES torrent_files(id) ON DELETE CASCADE,
    UNIQUE (parent_id, name),
    CHECK (
        (torrent_id IS NULL AND file_id IS NULL) OR
        (torrent_id IS NOT NULL AND file_id IS NOT NULL)
    )
) STRICT;

INSERT INTO nodes (id, parent_id, name, is_automatic) VALUES (1, NULL, 'root', 0), (2, 1, 'downloads', 0);