# Minimoon Sync Server

Standalone LAN file sharing server for Minimoon Sync.

## CLI

```sh
cargo run -- /path/to/music
```

The command starts the sharing server, prints the hostname, LAN IP address, and the URL to enter in the iPhone app, then keeps running until Ctrl-C.

The default listening port is `41324`.

## HTTP API

- `GET /files` returns syncable files as JSON:

```json
[
  {
    "path": "Album/track.mp3",
    "size": 1234,
    "last_modified": 1710000000000
  }
]
```

- `GET /file-by-path?path=Album%2Ftrack.mp3` downloads a syncable file by relative path.
- `GET /file/...` serves files from the shared directory.

## Syncable Files

The server includes these extensions:

- `jpg`
- `jpeg`
- `png`
- `webp`
- `m4a`
- `mp3`
- `wav`
- `ogg`
- `opus`
- `flac`
- `lrc`

It excludes hidden files/directories and common Windows metadata files:

- `desktop.ini`
- `AlbumArtSmall*`
- `AlbumArt_{*}*`

`/file-by-path` only accepts relative child paths inside the shared directory and rejects traversal, hidden paths, and non-syncable files.

## Library Usage

```toml
[dependencies]
minimoon-sync-server = { git = "https://github.com/milesegan/minimoon-sync-server.git", tag = "v0.1.0" }
```

The library exposes `ServerConfig`, `run_server`, `list_files`, `preferred_bind_ip`, and the existing `FileInfo` JSON shape used by Minimoon Sync.
