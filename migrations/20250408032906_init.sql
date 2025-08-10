CREATE TABLE torrents (
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    hash BLOB NOT NULL,
    name TEXT NOT NULL,
    hidden INTEGER NOT NULL DEFAULT 0, -- whether to hide from the qbittorrent list
    state INTEGER NOT NULL,
    error_message TEXT,
    debrid_id INTEGER UNIQUE,
    magnet_uri TEXT NOT NULL,
    progress REAL NOT NULL DEFAULT 0.0,
    upload_speed INTEGER NOT NULL DEFAULT 0,
    download_speed INTEGER NOT NULL DEFAULT 0,
    ratio REAL NOT NULL DEFAULT 0.0,
    eta_secs INTEGER NOT NULL DEFAULT 0,
    seeds INTEGER NOT NULL DEFAULT 0,
    peers INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    category TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    checked_at INTEGER,
    finished_at INTEGER,

    UNIQUE (hash)
) STRICT;

CREATE TABLE torrent_files (
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    debrid_id INTEGER NOT NULL,
    torrent_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,

    FOREIGN KEY (torrent_id) REFERENCES torrents(id) ON DELETE CASCADE,
    UNIQUE (torrent_id, path),
    UNIQUE(torrent_id, debrid_id)
) STRICT;

CREATE TABLE nodes (
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER,
    name TEXT NOT NULL,
    size INTEGER NOT NULL DEFAULT 0,
    readonly INTEGER NOT NULL,
    torrent_id INTEGER,
    file_id INTEGER,
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

INSERT INTO nodes (id, parent_id, name, readonly) VALUES 
    (1, NULL, 'root', 1), 
    (2, 1, 'downloads', 1);