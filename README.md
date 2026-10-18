# TableMinus

Personal database client for macOS and Linux: Tauri 2, a Rust core, a plain TypeScript UI.

- Engines: PostgreSQL (`tokio-postgres`, TLS via the OS), MySQL/MariaDB (`mysql_async`), SQLite (`rusqlite`, bundled).
- Connections live in `connections.json` in the app config folder; passwords live in the OS keychain (macOS Keychain, Linux Secret Service), never in the file.
- Safe mode: any query that `sqlparser` can't prove read-only asks for confirmation before it runs.

## Run

```sh
npm install
npm run tauri dev        # app with hot reload
npm run check            # tsc, rustfmt, clippy (warnings are errors), tests
npm run audit            # cargo-deny and cargo-audit (needs the network)
npm run bundle           # check, audit, MCP bridge, then .app/.dmg on macOS (.deb/AppImage on Linux)
```

`npm run bundle -- --skip-checks` skips the checks; `-- --target <triple>` cross-builds. Bundles land in `src-tauri/target/release/bundle/` (`target/<triple>/release/bundle/` when cross-building), with the `tableminus-mcp` bridge next to the app's executable.

Rust comes from `rustup`, Node from anywhere, at 22.12 or later (Vite's bundler needs it). Oracle connections need [Oracle Instant Client](https://www.oracle.com/database/technologies/instant-client.html) (Basic or Basic Light, 19 or later), at run time only: the app loads it from `ORACLE_CLIENT_LIB_DIR` when set, else from an `instantclient_*` folder in Downloads (where its installer puts it), your home folder, `/opt/oracle` or `/usr/local/lib`, else from the system's library path. On Linux, Tauri also needs `libwebkit2gtk-4.1-dev`, `build-essential`, `libssl-dev`, `librsvg2-dev`, and `libdbus-1-dev` for the keychain.

## Release

1. Set the version in `src-tauri/tauri.conf.json` (and `package.json`, `src-tauri/Cargo.toml`), and rename the
   changelog's Unreleased section to it.
2. `npm run bundle`. With the updater's signing key at `~/.tauri/tableminus.key` (or in `TAURI_SIGNING_PRIVATE_KEY`)
   the bundle folder also gets the signed update file (`macos/TableMinus.app.tar.gz` and its `.sig`) and
   `latest.json`. Its public key is in `tauri.conf.json`; a lost private key means installed copies can't update.
3. To sign with a Developer ID instead of ad hoc, set `APPLE_SIGNING_IDENTITY`; add `APPLE_ID`, `APPLE_PASSWORD` (an
   app-specific password) and `APPLE_TEAM_ID` to have Tauri notarize it too.
4. Make a GitHub release tagged `v<version>` with the `.dmg`, the update file, its `.sig` and `latest.json`. The app
   checks `releases/latest/download/latest.json`; beta versions go to a release tagged `beta`.

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
