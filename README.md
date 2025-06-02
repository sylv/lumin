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
- Torznab proxy to inject cached states into responses
- [TorBox](https://torbox.app) support too

## usage

this section is rough while i figure things out

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

### connecting to sonarr

1. Go to `Settings > Download Clients`
2. Click the plus button
3. Click `qBittorrent`
4. Change the port to match lumin (8000 by default) and the host
5. Change the "Category" to "sonarr" or "radarr" exactly
   1. If you get an error similar to "Failed to authenticate with qBittorrent" its likely because the category does not match the labels configured in lumin. If sonarr can't create the label it fails with an auth error.
6. Click "Test" then "Save"

### proxying torznab

> [!WARNING]
> This is *only* necessary if you want to prioritize cached content in sonarr or radarr.

if you want to prioritise cached content in sonarr or radarr, you need to proxy the torznab requests to prowlarr or jackett so it can inject cached states into the responses.

1. In Prowlarr, go to `Settings -> Apps`
2. Click the app you want to be proxied through Lumin and copy the "Prowlarr Server" URL
3. URL encode the "Prowlarr Server" URL using [an online tool like this](https://www.urlencoder.org/)
4. Replace the "Prowlarr Server" URL with `LUMIN_URL/torznab/<value>`, for example `http://lumin:8000/torznab/http%3A%2F%2Fprowlarr%3A9696`
5. Click "Test" then "Save" 
6. Click "Sync App Indexers"

at this point you'll want to open sonarr and manually search for something. it should work and should show some results with `[CACHED]` at the start of the name, indicating they are cached. you can use that to change how you rank torrents, if you want to prefer cached content.

### notes

- running fuse under docker can be tricky, `/mnt:/mnt:rshared` seems to avoid most of the downfalls but prepare yourself for that.
- the cache uses sparse files which will usually show as the full file size, even if the physical size is much smaller.
- specifying `LUMIN_TORBOX_PASSWORD` and `LUMIN_TORBOX_USERNAME` can slightly increase performance if you have high ping to torbox, but webdav can cause issues and is less reliable than the CDN directly. 

## todo

- Usenet support
- If torrents are downloading, increase the reconciler interval (based on eta?)
- Button to delete unused debrid torrents
- Handle debrid errors better
- Predictive preloading
  - Extract the season/episode number from the file and find the next episode, then pre-load a more significant portion of that file for "instant" playback.
  - Should only trigger when you're say, 80%-95% of the way through the current episode.
- When files are deleted, delete the cached data for them immediately instead of at the next cache sweep
- Support percentages for cache max/target sizes
  - "80%" would let the cache use 80% of the (total/available? maybe "%T"/"%A"?) space
- "On Demand" torrents that are added to debrid on read
  - This would be incompatible with plex, probably sonarr/radarr, and probably jellyfin as they read files on scan.
  - Usenet may be fast enough to not need them to be cached ahead of time