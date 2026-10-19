# Development

Every command here is from `package.json`, `scripts/` or the CI workflow. `npm run check` was run on 2026-10-18 at `12be348` and passed: 120 TypeScript tests; 392 Rust tests passed and 71 ignored (the ignored ones need live servers).

## Prerequisites

- **Rust** from rustup (stable). The owner's machine has Homebrew's rustup at `/opt/homebrew/opt/rustup/bin`, which non-login shells may not have on `PATH` (see Common failures).
- **Node** 22.12 or later (Vite 8 needs it). CI uses Node 24.
- **macOS:** Xcode command line tools. **Linux:** `libwebkit2gtk-4.1-dev`, `build-essential`, `libssl-dev`, `librsvg2-dev`, and `libdbus-1-dev` for the keychain.
- **Optional, at run time only:**
  - Oracle Instant Client (Basic or Basic Light, 19 or later) for Oracle connections. It's found through `ORACLE_CLIENT_LIB_DIR`, then an `instantclient_*` folder in Downloads, the home folder, `/opt/oracle` or `/usr/local/lib`, then the system library path.
  - `pg_dump`/`pg_restore`/`psql` and `mysqldump`/`mysql` for backup and restore. They're looked for on `PATH`, Homebrew, Postgres.app and DBngin.
  - Docker (the owner uses OrbStack) for the test servers.
- **For `npm run audit`:** `cargo install --locked cargo-deny cargo-audit`. The audit needs the network.

## Everyday commands

```bash
npm install
```

```bash
npm run tauri dev
```

Runs the app with hot reload: Vite on port 1420, then `cargo run`. The Rust side rebuilds when `src-tauri/` changes.

```bash
npm run check
```

Runs `tsc --noEmit`, `node --test 'src/**/*.test.ts'`, `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked`. It must pass before every commit (REQ-092).

```bash
npm run audit
```

Runs `cargo deny check` and `cargo audit`. Run it before a release and after adding or upgrading a dependency.

```bash
npm run bundle
```

Runs `scripts/build.sh`: check, audit (skipped with a note when the tools aren't installed), builds the `tableminus-mcp` bridge, then `tauri build`. Bundles land in `src-tauri/target/release/bundle/` (`.app` and `.dmg` on macOS, `.deb` and AppImage on Linux). Options: `-- --skip-checks`, `-- --target <triple>`.

CI (`.github/workflows/check.yml`) runs `npm ci`, `npm run build` (Tauri embeds `dist/`, so the frontend must exist before cargo runs) and `npm run check` on macOS.

## Environment variables

None are needed for development. These change behaviour when set:

| Variable | Used by | Purpose |
| --- | --- | --- |
| `ORACLE_CLIENT_LIB_DIR` | `sql/driver/oracle.rs` | Folder of Oracle Instant Client |
| `TABLEMINUS_MCP_TOKEN` | `tableminus-mcp` bridge | Use a narrower MCP token than the bridge's own |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PATH`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `scripts/build.sh` | Updater signing key; default file `~/.tauri/tableminus.key` |
| `APPLE_SIGNING_IDENTITY`; `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` (or `APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH`) | `scripts/build.sh`, Tauri | Developer ID signing and notarization. Wired in but never tried |
| `DBM_PG_URL`, `DBM_MSSQL_URL`, `DBM_CLICKHOUSE_URL` | ignored tests | Point live tests at a server other than the default local one |
| `TAURI_DEV_HOST` | `vite.config.ts` | Dev server host for device testing (Tauri template default) |

## Tests against real servers

Tests that need a database are `#[ignore]`d with a reason saying what they need ("needs a local MySQL; creates and drops a scratch database"). They don't run in `npm run check` or CI.

```bash
scripts/test-env.sh up
```

Starts Docker containers from `scripts/test-env.yml`: an sshd on 127.0.0.1:2222 that reaches Postgres 16 and MySQL 8.0 only through the tunnel, plus CockroachDB, SQL Server 2022, ClickHouse 24.8, MongoDB 8.0, Cassandra 5, Oracle 23 Free, the BigQuery emulator, Elasticsearch 8.15, DynamoDB Local and libSQL on local ports. The script prints each server's address and test credentials, and keeps its SSH key in `.test-env/` (git-ignored). Stop and wipe them with `scripts/test-env.sh down`.

```bash
cd src-tauri && cargo test --locked -- --ignored
```

Runs every ignored test. Several also expect servers on the usual local ports (Postgres on 5432, MySQL on 3306, Redis on 6379). The owner runs those with DBngin, not the compose file. Run single tests by name to target one engine. `scripts/test-ssh.sh` tests the SSH tunnel against MySQL.

```bash
npm run test:live
```

Runs the ignored tests that need MySQL, Postgres or Redis on their usual ports (`scripts/test-live.sh`). Whichever of the three isn't listening is started from the `live` profile of `scripts/test-env.yml` (passwordless, MySQL 8.0 with `local_infile`, Postgres with PostGIS); a local one such as DBngin is used as it is. It skips the tests that need `scripts/test-env.sh up`'s engines and the SSH tunnel test. The `live` job in `.github/workflows/check.yml` runs the same script against service containers (not yet run on GitHub).

## Checking the UI without the native window

Agents can't always drive the app window (the owner has denied computer-use access to it; see [agent-context/WORKFLOW.md](agent-context/WORKFLOW.md)). Past sessions checked the UI in the Vite dev server in a browser, with a stub for Tauri's IPC:

1. Write a temporary script that defines `window.__TAURI_INTERNALS__` with `invoke` (canned answers per command), `transformCallback` and `metadata`, and a copy of `index.html` that loads it in `<head>`.
2. Start Vite (`.claude/launch.json` has a `vite` entry on port 1420) and open the copy.
3. Delete both files afterwards. Never commit the stub.

This checks layout and UI logic, not the backend. TODO.md separates "tried in the browser preview" from "tried in the app window"; keep that distinction.

## Release

Steps from [../README.md](../README.md):

1. Set the version in `src-tauri/tauri.conf.json`, `package.json` and `src-tauri/Cargo.toml`, and rename the changelog's Unreleased section to that version.
2. `npm run bundle`. With the signing key present, the bundle folder also gets the signed update file and `latest.json`, whose notes come from `CHANGELOG.md` (`scripts/latest-json.mjs`).
3. Optionally sign with a Developer ID (variables above).
4. Make a GitHub release tagged `v<version>` with the `.dmg`, the update file, its `.sig` and `latest.json`. Beta builds go to a release tagged `beta`.

No release has been published yet (TASK-004). Publishing is outward-facing: it needs the owner's explicit go-ahead each time (REQ-091). Losing the private key at `~/.tauri/tableminus.key` means installed copies can never update.

## Common failures

| Symptom | Cause | Fix |
| --- | --- | --- |
| `failed to run 'cargo metadata' … No such file or directory` | cargo isn't on `PATH` (Homebrew rustup) | The owner added `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` to `~/.zshenv` (2026-10-14). `scripts/build.sh` also searches `~/.cargo/bin` and rustup toolchains |
| `Error: Port 1420 is already in use` | Another Vite (often an agent's browser preview) holds the port | Stop it. Past sessions used a temporary `.claude/launch.json` entry on another port and reverted it before committing |
| A shared `.app` says it is "damaged" on another Mac | The bundle wasn't ad-hoc signed as a whole | Fixed: `tauri.bundle.json` has `"signingIdentity": "-"`. Recipients still need System Settings › Privacy & Security › Open Anyway, or `xattr -dr com.apple.quarantine /Applications/TableMinus.app`. Send the `.dmg`, not a zipped `.app`. Only Developer ID signing and notarization remove the prompt |
| MySQL TLS panics about the crypto provider | Both rustls providers are compiled in | `db::install_tls_provider()` installs ring once; it is called before each rustls config is built (`sql/db.rs`, `connect/tls.rs`, `app/updater.rs`). New TLS code must call it too |
| The first build takes many minutes | DuckDB compiles from source | Expected |
| Oracle connection says where to get Instant Client | Instant Client isn't installed or found | Install it, or set `ORACLE_CLIENT_LIB_DIR` |
| An Oracle connection waits 20 s, then "no answer from the server", from the app but not from tests | Instant Client looks up this Mac's own `.local` name over multicast DNS, which macOS's Local Network privacy holds back from an app until the user allows it | Allow TableMinus under System Settings › Privacy & Security › Local Network (the app asks, through `NSLocalNetworkUsageDescription` in `Info.plist`) |
| Each rebuilt dev `.app` asks for keychain access again | An unsigned build is a new app to the keychain | Expected for `--no-sign` builds: allow it, or sign the build |
| `npm run tauri dev` app can't be driven by computer-use | The dev binary isn't an `.app` bundle | Build a debug `.app` (`npm run tauri build -- --debug`) or use the browser preview |
