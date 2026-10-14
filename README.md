# Table Minus

Personal database client for macOS and Linux: Tauri 2, a Rust core, a plain TypeScript UI.

- Engines: PostgreSQL (`tokio-postgres`, TLS via the OS), MySQL/MariaDB (`mysql_async`), SQLite (`rusqlite`, bundled).
- Connections live in `connections.json` in the app config folder; passwords live in the OS keychain (macOS Keychain, Linux Secret Service), never in the file.
- Safe mode: any query that `sqlparser` can't prove read-only asks for confirmation before it runs.

## Run

```sh
npm install
npm run tauri dev        # app with hot reload
npm run check            # tsc, rustfmt, clippy (warnings are errors), tests
npm run tauri build      # .app/.dmg on macOS; .deb/AppImage on Linux
```

Rust comes from `rustup`. On Linux, Tauri also needs `libwebkit2gtk-4.1-dev`, `build-essential`, `libssl-dev`, `librsvg2-dev`, and `libdbus-1-dev` for the keychain.

## Layout

| File | Role |
| --- | --- |
| `src-tauri/src/db.rs` | one `Conn` enum over the three engines: open, query (text cells), catalog, inline edits |
| `src-tauri/src/error.rs` | the error type every command returns |
| `src-tauri/src/readonly.rs` | safe-mode read-only check |
| `src-tauri/src/store.rs` | saved connections + keychain passwords |
| `src-tauri/src/lib.rs` | Tauri commands |
| `src/main.ts`, `index.html` | sidebar, SQL box, results table |

Roadmap: [TODO.md](TODO.md). Coding rules: [CLAUDE.md](CLAUDE.md).
