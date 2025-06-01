-- this was swapped to using error states with messages
DROP TABLE torrent_blocks;

-- this is used for torrents that are deleted but still have nodes referencing them
ALTER TABLE torrents ADD COLUMN orphaned INTEGER DEFAULT 0 NOT NULL;

-- immutable is a much better name for this
ALTER TABLE nodes RENAME COLUMN is_automatic TO immutable;
-- swap the root and downloads nodes to immutable
UPDATE nodes SET immutable = 1 WHERE id IN (1, 2);