# Data model

TableMinus has no database of its own. Its state is JSON files in the app's config folder (some movable through Settings › Locations), secrets in the OS keychain, and in-memory sessions. The types are in `src-tauri/src/connect/store.rs`, `sql/db.rs`, `connect/tls.rs`, `connect/ssh.rs`, `safety.rs`, `app/locations.rs` and `assistant/mcp.rs`.

## Where files live

The config folder is Tauri's `app_config_dir` for the identifier `dev.ritesh.dbmanager`: `~/Library/Application Support/dev.ritesh.dbmanager/` on macOS, and `$XDG_CONFIG_HOME/dev.ritesh.dbmanager/` (usually `~/.config/…`) on Linux.

| File | Contents | Location (Settings › Locations) |
| --- | --- | --- |
| `connections.json` | `{ connections, groups, tags }`: saved connections, launcher groups, custom tags. Older builds wrote a bare array, which still loads. | Connections folder |
| `sql-tabs.json` | Kept SQL tabs per connection and database | Queries folder |
| `favorites.json` | Saved queries, in folders, per connection | Queries folder |
| `metrics.json` | Metrics boards per connection | Queries folder |
| `history.json` | Query history per connection (at most 5,000 each; retention setting) | History folder |
| `ai-chats.json` | Assistant chats per connection | History folder |
| `settings.json` | Settings, themes, column layouts, other remembered UI choices | Settings folder |
| `locations.json` | Which of the folders above are moved, e.g. to iCloud Drive or Dropbox | Always the config folder |
| `mcp.json` | MCP server settings, access tokens (as SHA-256 hashes), activity | Always the config folder: tokens belong to one machine (DEC-015) |
| `mcp-bridge.json` | The port and token the stdio bridge uses; mode 0600; written while the server runs | Always the config folder |
| `plugins.json`, `plugins/<id>/` | Installed plugins and their files | Always the config folder |
| `secrets.vault` | Linux only, when there is no Secret Service: every secret, sealed (below) | Always the config folder |

All of these are written to a temporary file and renamed into place, and a file that fails to load is never overwritten (`locations::write` refuses; REQ-012).

## A saved connection (`store::Saved`)

`#[serde(deny_unknown_fields)]`, so a file with unknown fields is refused rather than silently rewritten.

| Field | Meaning |
| --- | --- |
| `name` | Unique; also the keychain account and the base of the session key |
| `kind` | `db::Engine`: `postgres`, `mysql`, `mariadb`, `sqlite`, `duckdb`, `mssql`, `clickhouse`, `libsql`, `d1`, `redis`, `mongodb`, `dynamodb`, `elasticsearch`, `cassandra`, `oracle`, `bigquery` |
| `url` | Connection URL **without the password** (a file path for SQLite and DuckDB) |
| `tag` | `""`, `local`, `development`, `staging`, `production`, or a custom tag |
| `safety` | `safety::Safety`: `silent`, `alert1`, `alert2` (default), `safe1`, `safe2`. Old files with `confirm_writes: bool` still load (true is Alert 2) |
| `ssh` | `{ host, port, user, key }`; blank `key` means `~/.ssh/config`, the agent, then default keys |
| `tls` | `{ mode, ca, cert, key }`; modes Disable / Prefer / Require / Verify CA / Verify full, with libpq meanings |
| `options` | `db::Options`: startup SQL, startup script, Postgres direct TLS, no channel binding, system schemas, a Unix socket, MySQL SNI and cleartext auth |
| `password_mode` | `keychain` (default), `ask` (held in memory until the workspace closes), `no_password` |
| `color` | Status colour, one of five swatches |
| `group` | Launcher group name; empty when ungrouped |
| `flavor` | A Postgres-compatible server it's labelled as (`cockroach`); it still connects as `kind` |
| `needs_credentials` | Set by discovery when the default sign-in failed; only the backend sets or clears it, on the first successful connect |

## Secrets (keychain)

| Service | Account | Holds |
| --- | --- | --- |
| `dev.ritesh.dbmanager` | connection name | Database password, SQLite/SQLCipher passphrase, libSQL/D1 tokens, DynamoDB secret key |
| `dev.ritesh.dbmanager.ssh` | connection name | SSH password or private-key passphrase |
| `dev.ritesh.dbmanager.passcode` | fixed account | argon2 hash of the app passcode |
| `dev.ritesh.tableminus.ai` | provider | AI provider API keys, proxy password |

The `dev.ritesh.dbmanager` names predate the TableMinus name and must stay: renaming them would orphan every saved password (DEC-018).

**Linux without a Secret Service:** the same entries go into `secrets.vault`, one JSON map sealed with AES-256-GCM under a key from argon2id and a master password, typed once per session. Until then reads and writes fail with `vault_locked`, and the UI asks (`src/shell/vault.ts`). "Forgot master password?" starts a new vault and keeps the old file renamed.

**Exported connection files:** sealed the same way (AES-256-GCM, argon2id key from a file password). Passwords go only into a password-protected file. An imported connection that carries a startup script is held back until the user has seen the script and chosen to keep or drop it.

## Sessions (in memory)

- **Session key:** the saved connection's name for a workspace's first database tab; `name\u{1f}n` for further database tabs of the same connection. Every command's `name` argument is such a key (`sessions::connection(key)` gets the name back).
- **SQL sessions** (`Open.conns`): `Live { saved, url, password, conn, tunnel }`, one `Mutex` each.
- **Other engines:** `sessions::Sessions<S>` per engine, under the same keys.
- **Safe-mode batch grants:** SHA-256 digests of short-lived tokens (5 minutes), in memory only (`safety.rs`).
- **MCP:** connected clients and their request counts since the app started; nothing persists across launches except `mcp.json`.

## Rules not visible in the field names

- A connection's name is its identity across files, the keychain and sessions. Renaming goes through `save_connection` with `previous`, which drops open sessions under both names so the next connect uses the new settings.
- Changing a connection's safety level away from a Safe level wants that connection's password, in the form as on the toolbar lock (`may_change_safety`).
- Duplicating a connection copies its keychain secrets; "Copy as URL" never includes the password.
- Discovery saves new connections under free names with the Local tag, never replaces existing ones, and puts nothing in the keychain.
