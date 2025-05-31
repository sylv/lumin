# lumin

> [!WARNING]
> lumin is not stable and will break in weird ways. if you use it, understand that the cache and in worse scenarios the database itself may have to be reset.
 
lumin is a debrid proxy that handles streaming files from debrid services like TorBox using FUSE and provides a qBittorrent-compatible API to interact with them.

## features

- Aggressive caching
- Creating folders and hardlinks on the mount
- Intelligent prefetching based on bitrate
- qBittorrent-compatible API
- (Very early) Web UI to manage torrents
- Some other stuff I'm probably forgetting
- It works with sonarr at least
- [TorBox](https://torbox.app) support too

## setup

yeah wouldnt you like to know huh

ᶦˡˡ ᵈᵒ ᵗʰᶦˢ ˡᵃᵗᵉʳ

heres a rough example that **does not include auth**

```yml
services:
  lumin:
    container_name: lumin
    image: docker.io/sylver/lumin
    restart: unless-stopped
    healthcheck:
      test: curl --connect-timeout 10 --silent --show-error --fail http://localhost:8000/trpc/get_torrents
      timeout: 60s
      interval: 30s
      retries: 4
    cap_add:
      - SYS_ADMIN
    devices:
      - /dev/fuse
    security_opt:
      - apparmor:unconfined
    ports:
      - 127.0.0.1:8000:8000 # webui + qbittorrent-compatible api
    volumes:
      - /mnt:/mnt:rshared
      - ./.lumin:/data # this includes the cache path and database
    environment:
      - RUST_LOG=lumin=debug
      - LUMIN_ALLOW_OTHER=true # whether other users can access the mount
      - LUMIN_MOUNT_UNPRIVILEGED=true
      - LUMIN_MOUNT_PATH=/mnt/lumin
      - LUMIN_TORBOX_KEY=${TORBOX_KEY}
      # these are optional, required for webdav.
      # without these, the api is used which may increase ttfb.
      - LUMIN_TORBOX_USERNAME=${TORBOX_USERNAME}
      - LUMIN_TORBOX_PASSWORD=${TORBOX_PASSWORD}
```

### notes

- add as a "qBittorrent" download client in sonarr on port 8000
  - labels must be used by sonarr/radarr or else there can be issues with download management
  - if the labels in sonarr/radarr do not match the labels configured in lumin, you will get an error trying to add it as a download client because the label creation will fail.
- running fuse under docker can be tricky, `/mnt:/mnt:rshared` seems to avoid most of the downfalls but prepare yourself for that.
- the cache uses sparse files which will usually show as the full file size, even if the physical size is much smaller.

## todo

- Usenet support
- If torrents are downloading, increase the reconciler interval
- Button to delete unused debrid torrents
- Instead of blocking torrents, mark as errored and add error message with reason
- If a torrent contains only .lnk/.exe/etc files, block it.
- Predictive preloading
  - Extract the season/episode number from the file and find the next episode, then pre-load a more significant portion of that file for "instant" playback.
  - Should only trigger when you're say, 80%-95% of the way through the current episode.
- When files are deleted, delete the cached data for them immediately instead of at the next cache sweep
- Support percentages for cache max/target sizes
  - "80%" would let the cache use 80% of the (total/available? maybe "%T"/"%A"?) space
- "On Demand" torrents that are added to debrid on read
  - This would be incompatible with plex, probably sonarr/radarr, and probably jellyfin as they read files on scan.
  - Usenet may be fast enough to not need them to be cached ahead of time