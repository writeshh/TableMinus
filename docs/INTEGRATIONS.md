# Integrations

Everything outside the app that TableMinus talks to. Per-engine feature depth and test history are in [../TODO.md](../TODO.md) §24; this page covers how each connection works, its limits and how to test it locally. No credentials appear here. Test-server credentials are printed by `scripts/test-env.sh up` and live only in `scripts/test-env.yml`.

## Database engines

All connections can go through an SSH tunnel (`ssh.rs`, russh; known_hosts accept-new), and TLS follows the libpq modes everywhere: Prefer and Require encrypt without checking the certificate, Verify CA checks the chain, Verify full the chain and the host name. A connect gives up after `CONNECT_TIMEOUT` (20 s).

| Engine | Client | Protocol and auth | Read-only enforced by the server? | Local test server |
| --- | --- | --- | --- | --- |
| PostgreSQL | tokio-postgres | Wire protocol, password or SCRAM, Unix socket, direct TLS | Yes (read-only transaction) | DBngin on 5432; compose Postgres 16 behind sshd |
| CockroachDB | Postgres driver, `flavor: cockroach` | As Postgres | Yes | compose, 26257 |
| MySQL, MariaDB | mysql_async | Wire protocol; socket; SNI; cleartext plugin opt-in | Yes | DBngin on 3306; compose MySQL 8.0 behind sshd |
| SQLite, SQLCipher | rusqlite | A file; passphrase or raw key in the keychain | Yes (`query_only`) | none needed |
| DuckDB | duckdb (bundled) | A file or `:memory:` | Yes (read-only transaction) | none needed |
| SQL Server | tiberius | TDS, SQL login only; a named instance's port from SQL Browser (UDP 1434) | **No**: rests on the SQL parse | compose 2022 (amd64, emulated on Apple silicon) |
| ClickHouse | own HTTP client | HTTP(S) 8123/8443, one `session_id` per connection | Yes (`readonly=1` per request) | compose 24.8 |
| libSQL / Turso | own HTTP client | Hrana 3 over HTTP, bearer token | **No** (`sqld` refuses `query_only`) | compose `sqld` on 8080 |
| Cloudflare D1 | own HTTP client | Cloudflare API with an API token, account and database IDs | **No** | none: tested against a fake server only |
| Oracle | oracle (ODPI-C) | Easy Connect Plus; `tcps` with system certificates or a wallet | Yes (`SET TRANSACTION READ ONLY`) | compose Oracle 23 Free; Instant Client needed on the host |
| BigQuery | own HTTP client + `ring` | REST (`jobs.query`, `getQueryResults`); a service account's JSON key signs an OAuth token request; the emulator takes none | **No**: rests on the SQL parse | compose BigQuery emulator on 9050 (no key) |
| Redis | own RESP2 client | TCP or TLS, AUTH with an ACL user | Command list (`redis::is_read_only`) | local Redis on 6379 |
| MongoDB | mongodb | `mongodb://` and `+srv`, TLS with CA and client PEM | Operation kinds from the shell recording | compose 8.0 on 27018 |
| DynamoDB | own HTTP client + SigV4 | Access key and secret, region; DynamoDB Local over plain HTTP | Statement class (only SELECT reads) | compose DynamoDB Local on 8001 |
| Elasticsearch, OpenSearch | own HTTP client | Basic auth or API key; TLS | Request class (SQL, GET, HEAD, search endpoints read) | compose 8.15 on 9200, security off |
| Cassandra, ScyllaDB | scylla | CQL native protocol, password, TLS through rustls | Statement class (SELECT, DESCRIBE, LIST read) | compose Cassandra 5 on 9042 |

Notable failure modes, all from TODO.md and commit history:

- **Postgres over SSH** once failed with "invalid number of ports"; fixed in `3df5abd`, found through the Docker sshd.
- **MySQL over a Unix socket** with a TLS setting failed with "Packets out of sync"; a socket connection now ignores TLS.
- **MySQL `LOAD DATA LOCAL INFILE`:** the server picks the file, so the client sends only the file the statement itself names, once.
- **ClickHouse** holds a session's lock until the server notices a hang-up, so the next request may wait a few seconds after a cancelled one.
- **Oracle** needs Instant Client 19 or later on the user's machine; without it the connection explains where to get it. Bundling it with the app is open (TASK-011).
- **BigQuery** compares a column only with a value of its own type, and the app binds every value as text, so the SQL casts each to its column's type. A transaction exists only inside a script, so it's queued and sent at commit, as D1's is. Sign-in to Google is unit-tested only (the emulator needs no key).
- **D1** refuses `BEGIN`, so the driver queues a transaction's statements and sends them as one batch at commit.
- **Cassandra in Docker or through a tunnel:** every node address is replaced by the one dialled (address translation), or the driver would try the containers' internal addresses.

## External tools

| Tool | Used for | How |
| --- | --- | --- |
| `pg_dump`, `pg_restore`, `psql` | Postgres backup and restore | Found on `PATH`, Homebrew, Postgres.app, DBngin; the oldest version not older than the server wins. The password goes in `PGPASSWORD`, never in arguments |
| `mysqldump`, `mysql` | MySQL/MariaDB backup and restore | Same search; `MYSQL_PWD`; TLS and tunnels mirrored as flags. mysqldump has no connect-timeout flag (Cancel covers it) |
| `git` | Themes and plugins from a Git repository | Shallow clone, HTTPS only, no prompts, no LFS, two-minute limit |
| `docker ps`, `lsof` | Find Local Databases | Published ports and listening database processes |
| `/bin/sh -c` | A connection's startup script | The user's own script; nothing interpolated into it |
| Local AI agents (`claude`, `codex`, `gemini`, any command) | The assistant over ACP | Started without a shell; killed with their chat or at quit |

## AI providers

`src-tauri/src/ai.rs`. Off until the user turns it on under a consent notice (Settings › AI).

- **Anthropic** Messages API, and any **OpenAI-compatible** chat completions endpoint (OpenAI, Ollama, LM Studio…) with a custom base URL. HTTPS only, except to this computer.
- API keys and the proxy password are in the keychain (`dev.ritesh.tableminus.ai`).
- An optional HTTP proxy (CONNECT tunnel) with Basic credentials.
- The model sees the schema (tables, columns, types, foreign keys) and has one tool, `run_query`, which runs only on the user's approval, only when read-only, at most 50 rows.
- Not done: GitHub Copilot (TODO §19).

## MCP clients

`src-tauri/src/mcp/`. A loopback HTTP server for agents, plus the `tableminus-mcp` stdio bridge. Settings › MCP can add the bridge to Claude Desktop, Cursor and Windsurf config files, keeping the file's other entries and copying the original aside first. Details: [features/mcp-and-ai.md](features/mcp-and-ai.md).

## Updates

`src-tauri/src/updater.rs`, `src/updates.ts`. Tauri's updater reads `latest.json` from `https://github.com/writeshh/TableMinus/releases/latest/download/latest.json`, or from the release tagged `beta`. Updates are signed with the minisign key whose public half is in `tauri.conf.json`. The check runs quietly at startup unless Settings › General turns it off. It has never run against a real release (none published).

## OS services

- **Keychain:** macOS Keychain or the Linux Secret Service (`keyring`), with an encrypted vault file as the Linux fallback ([DATA-MODEL.md](DATA-MODEL.md)).
- **Touch ID:** `robius-authentication`, macOS only: safe mode, the passcode, batch grants.
- **File associations and URL schemes:** `.sqlite`/`.db`/`.duckdb`/`.sql` files (rank Alternate) and `postgresql://`, `mysql://`, `cockroachdb://`, `redis://`, `mongodb://`… links (`Info.plist`) open the matching form or the SQL viewer. Not tried in a bundled app; macOS only (Linux would need the deep-link plugin).
