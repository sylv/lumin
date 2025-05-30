# lumin

> [!WARNING]
> lumin is not stable and will break in weird ways. if you use it, understand that the cache and in worse scenarios the database itself may have to be reset.
 
lumin is a debrid proxy that handles streaming files from debrid services like Torbox using FUSE and provides a qBittorrent-compatible API to interact with them.

## features

- Aggressive caching
- Creating folders and hardlinks on the mount
- Intelligent streaming of files
- qBittorrent-compatible API
- Basic web UI
- Some other stuff I'm probably forgetting
- [TorBox](https://torbox.app) support

## setup

yeah wouldnt you like to know huh
ᶦˡˡ ᵈᵒ ᵗʰᶦˢ ˡᵃᵗᵉʳ

## notes

- labels must be used by sonarr/radarr or else there can be issues with download management

## todo

- Duration hint loading
- If torrents are downloading, increase the reconciler interval
- Button to delete unused debrid torrents
- Instead of blocking torrents, mark as errored and add error message with reason
- If a torrent contains only .lnk/.exe/etc files, block it.
- Backoff jitter
- Usenet support
- Predictive preloading
  - Extract the season/episode number from the file and find the next episode, then pre-load a more significant portion of that file for "instant" playback.
  - Should only trigger when you're say, 80%-95% of the way through the current episode.
- Purge torrents from cache on delete
- Support percentages for cache max/target sizes
  - "80%" would let the cache use 80% of the (total/available? maybe "%T"/"%A"?) space
- SQLite just sucks, something like [sled] would be better once it hits v1
  - Store cache meta in the database
- "On Demand" torrents that are added to debrid on read
  - This would be incompatible with plex, which reads files on scan.
  - Usenet may be fast enough to not need them to be cached ahead of time