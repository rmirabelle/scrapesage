# ScrapeSage repository instructions

Scrape Sage reads the web pages in the user's list, shows the videos it finds
as a thumbnail list with a search box, and downloads a video with one button.
Tauri 2 + React. The shell (title bar, menu bar, splash, About dialog, updater,
installer, release scripts) is the same as Kaleido Sage's.

## Layout

- `src/App.tsx` holds the screen: the page list (kept in `localStorage`), the
  search box (a filter on the video titles) and the video list.
- `src-tauri/src/scrape.rs` finds the videos of one page. It reads the HTML
  first (`parse_page`); when that shows no video, yt-dlp reads the page.
- `src-tauri/src/download.rs` downloads to `Videos\Scrape Sage`: a video file
  with a plain request, a video page or stream list with yt-dlp. It sends
  `download-progress` events.
- `src-tauri/src/ytdlp.rs` downloads the official `yt-dlp.exe` into the app's
  data folder on first use and checks its SHA-256. The app has no FFmpeg, so
  yt-dlp downloads the best single file (`--format b`).
- `src-tauri/src/updater.rs` checks `rmirabelle/scrapesage` GitHub releases.

## Release memory

- Read `docs/RELEASING.md` before changing installer configuration, preparing a
  release, or publishing.
- Preserve the shared XSage per-user installer contract. Keep
  `src-tauri/installer.nsi`, the bundled `icons/icon.ico` resource, and the
  Windows GUI-subsystem attribute intact.
- Use `set-version.ps1` and `publish.ps1`; do not manually create release tags or
  GitHub releases. A local build is not authorization to publish.

## Development process

- The dev app must run outside Claude's process tree, so it does not stop when
  Claude quits. Start it only with `start-dev.vbs`, opened through Explorer:
  `Start-Process "$env:WINDIR\explorer.exe" '"D:\Code\ScrapeSage\start-dev.vbs"'`.
  It runs `npm run tauri dev -- --no-watch` (Vite on port 14520) with no console
  window, and writes its output to `dev.log` in the project folder: read that
  file for build errors and for `[client] hot update applied: <file>`. Start or
  restart the dev app only when the user asks or agrees.
- Stop it with `.\kill-dev.ps1`. It closes the window, tree-kills what is left
  of `scrapesage.exe` and frees ports 14520 and 14521.
- Do not start the dev app while an installed Scrape Sage is running; the
  single-instance plugin would route the launch to it.
