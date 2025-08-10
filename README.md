# lumin

> [!WARNING]
> lumin is very unstable, assume you will discover bugs and will have to reset your database at some point.

lumin is a middleman for debrid services that mounts them as a local filesystem and provides a qBittorrent-compatible API to interact with them, like [rdt-client](https://github.com/rogerfar/rdt-client) but streaming files on demand instead of downloading them ahead of time. Sonarr and Radarr are supported using hardlinks within the fuse mount.

currently only [TorBox](https://torbox.app/subscription?referral=f6ae20d2-ccaa-43a8-9006-e0aac8cc8d71) is supported.

## usage

```yml
services:
  lumin:
    container_name: lumin
    image: docker.io/sylver/lumin
    restart: unless-stopped
    cap_add:
      - SYS_ADMIN
    devices:
      - /dev/fuse
    security_opt:
      - apparmor:unconfined
    ports:
      - 127.0.0.1:8000:8000 # qbittorrent api
    volumes:
      - /mnt:/mnt:rshared
      - ./.lumin:/data # this stores the cache data and database information
    environment:
      - LUMIN_MOUNT_PATH=/mnt/lumin
      - LUMIN_ALLOW_OTHER=true # whether other users can access the mount
      - LUMIN_MOUNT_UNPRIVILEGED=true
      - LUMIN_TORBOX_KEY=${TORBOX_KEY}
      # these are optional and will enable webdav.
      # recommended for slightly better performance.
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

## todo

- Usenet support
- Configurable uid/gid/mode
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
- "Passthrough" mounting, similar to mergerfs
  - Attach a source drive like `/mnt/pool`, files in it will show up in lumins mount
  - When remote files are streamed, we store the cache data in the source drive at eg `/mnt/pool/media/.../some_file.mkv.lpartial`
  - If the entire file is streamed (or configuration is set to download the full file), we move the file to `/mnt/pool/media/.../some_file.mkv` and detach it from lumin
  - This means you can start and stop using lumin easily without "losing" data, you could even "eject" by downloading everything lumin has and then removing it.
  - This would be cool for hybrid setups