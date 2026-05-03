<p align="center">
  <img src="public/readme-logo.png" alt="Miru" width="160" />
</p>

# Miru Desktop Client

Desktop companion for Miru's osu!mania rendering workflow. It handles Miru login, osu!stable replay watching, Auto Renderer filters, renderer downloads, benchmarking, and public render-worker mode.

> [!NOTE]
> The current client targets Windows.

## Features

- Watches osu!stable mania replays from `Data\r` and saved `Replays`.
- Sends matched replays to Miru with preset, skin, quality, and filter settings.
- Downloads and verifies the managed Miru Renderer binary from GitHub Releases.
- Lets eligible machines benchmark, register, and connect as public render workers.
- Includes tray behavior, Windows autostart, local history, and runtime logs.

## Requirements

- Windows 10/11
- Node.js 22+
- Rust 1.77.2+
- Tauri v2 Windows build prerequisites
- `ffmpeg` and `ffprobe` on `PATH` for worker benchmarks/render jobs
- osu!stable installed locally, or an osu!stable path override in Settings

## Development

```bash
npm install
npm run tauri:dev
```

Useful commands:

```bash
npm run lint
npm run build
npm run tauri:build
```

The frontend lives in `src/`. The Tauri/Rust app lives in `native/`.
