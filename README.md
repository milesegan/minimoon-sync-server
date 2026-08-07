# Minimoon Sync Server

Standalone LAN file sharing server for Minimoon Sync.

## CLI

Install from crates.io:

```sh
cargo install minimoon-sync-server
```

Run the server:

```sh
minimoon-sync-server /path/to/music
```

Run directly from a source checkout:

```sh
cargo run -- /path/to/music
```

The command starts the sharing server, prints a summary table with the number and total size of syncable files, hostname, LAN IP address, and the URL to enter in the iPhone app, then keeps running until Ctrl-C.

The default listening port is `41324`.
The server listens on all IPv4 interfaces and advertises the preferred private LAN address for the iPhone app.

Optional flags:

- `--verbose` or `-v` enables request and path-resolution debug logging.
- `--allow-top-level-directory-symlinks` lets top-level directory symlinks inside the shared directory point to syncable files outside the shared directory. This is off by default.

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
- `GET /file/...` serves files from the shared directory. This legacy endpoint is deprecated and will be removed in a future update. Use `/file-by-path?path=...` for all new clients.

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

`/file-by-path` only accepts relative child paths inside the shared directory and rejects traversal, hidden paths, and non-syncable files. By default, symlinks that resolve outside the shared directory are rejected. When the CLI is started with `--allow-top-level-directory-symlinks`, files under a top-level directory symlink are included in `/files` and can be downloaded through `/file-by-path`; direct file symlinks that point outside the shared directory remain rejected.

## Library Usage

```toml
[dependencies]
minimoon-sync-server = "0.1"
```

The library exports `ServerConfig`, `run_server`, `iter_files`, `preferred_bind_ip`, and the `FileInfo` JSON shape used by Minimoon Sync. `iter_files` reads entries as they are requested instead of allocating the full list first. To allow top-level directory symlinks, use `ServerConfig::with_allow_top_level_directory_symlinks(true)` or pass `FileAccessOptions` to the option-aware path helpers.

For unreleased changes, depend on Git directly:

```toml
[dependencies]
minimoon-sync-server = { git = "https://github.com/milesegan/minimoon-sync-server.git" }
```
