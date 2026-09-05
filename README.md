# 1DM for Windows

A lightweight, 1DM-style download manager for Windows: an embedded browser
(Edge WebView2 — already on every Windows 10/11 machine, so nothing extra is
shipped), stream detection, and a multi-connection download engine.

## Status

v0.1 scaffold: project skeleton, download engine (multi-connection HTTP + HLS),
Tauri 2 app shell, frontend, and the CI/CD pipeline that publishes versioned
installers to GitHub Releases.

## Roadmap

| Phase | Contents |
|-------|----------|
| 0     | ✅ Scaffold + CI/CD (tag push → versioned GitHub Release) |
| 1     | Embedded browser, stream detection (m3u8/mpd), download queue, MP4 output |
| 2     | Multi-connection for regular files, clipboard link detection, quality picker |
| 3     | Scheduler, batch/website grabber, torrents |
| 4     | Polish: themes, in-browser adblock, "play protected content" permission dialog |

## DRM note

Like 1DM, yt-dlp, and every legitimate tool: streams served with Widevine DRM
can be **played** in the embedded browser but **cannot be downloaded**. Content
served as plain HLS/DASH (most TV-series content on Sony Liv, historically)
downloads at full quality. The app surfaces the `isEncrypted` flag from the
player API so you know before you start.

## Development

Prerequisites: Rust (MSVC toolchain), Node 20+, and the WebView2 runtime
(preinstalled on Windows 10/11).

```bash
# Frontend
cd ui && npm install && npm run dev

# App (from repo root)
cargo run --manifest-path src-tauri/Cargo.toml
```

The frontend and Rust shell are wired: frontend calls `invoke("start_download")`
etc. and listens to `download://progress` / `download://finished` events.

## Releases

Push a version tag and CI publishes an installer to GitHub Releases:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Bump the version in `src-tauri/tauri.conf.json` (and `Cargo.toml` files) before
tagging. CI also runs checks on every push/PR (`cargo check`, frontend build,
full `tauri build --no-bundle`).

## Disclaimer

This is a general-purpose download manager. Respect the terms of service and
copyright of the sites you download from; downloading may be prohibited by a
site's ToS or the laws of your country.