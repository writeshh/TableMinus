# Tech stack

Versions are what `Cargo.lock` and `package-lock.json` held on 2026-10-18. The manifests (`src-tauri/Cargo.toml`, `package.json`) carry a comment on most dependencies saying why they are there; read those before adding or swapping one.

## Platform

| | Version | Notes |
| --- | --- | --- |
| Tauri | 2.12.3 (`@tauri-apps/api` 2.12.3, CLI 2.12.2) | Plugins: dialog, opener, updater (without its `zip` feature) |
| Rust | edition 2024; built with rustc 1.99 (stable, via rustup) | No `rust-version` pinned |
| Node | 22.12 or later (Vite 8's bundler); CI uses 24 | Runs the TS tests with `node --test`, which strips types itself |
| TypeScript | 6.0.3, `strict`, no emit | Plain DOM code, no UI framework |
| Vite | 8.3.4 | Dev server on port 1420 (`strictPort`) |

## Rust core

| Area | Crates | Why (from the manifests and history) |
| --- | --- | --- |
| Async | tokio 1.53, async-trait, futures-util | `Driver` is a trait object with async methods |
| Postgres, CockroachDB | tokio-postgres 0.7.18, postgres-protocol, postgres-native-tls | Text protocol; OS TLS; SCRAM verifiers for user passwords |
| MySQL, MariaDB | mysql_async 0.37.1 (rustls + ring) | Pure Rust, so no bundled client libraries (TablePlus ships two) |
| SQLite, SQLCipher | rusqlite 0.40.2 (`bundled-sqlcipher`, `backup`) | CommonCrypto on macOS, system libcrypto on Linux |
| DuckDB | duckdb 1.10506 (`bundled`) | Built from source: a long first compile and a larger app |
| SQL Server | tiberius 0.13 (rustls) | |
| ClickHouse, libSQL, D1, DynamoDB, Elasticsearch | hyper 1 + the app's own small HTTP client (`driver/http.rs`) | DynamoDB's SigV4 signing is hand-written (`dynamo/sigv4.rs`) instead of the AWS SDK |
| MongoDB | mongodb 3.9.1 | Needs MongoDB 4.0 or later |
| Cassandra, ScyllaDB | scylla 1.9 (rustls) | |
| Oracle | oracle 0.6.3 (ODPI-C) | Needs Oracle Instant Client at run time, not at build time (DEC-009) |
| BigQuery | the HTTP client above + ring 0.17 | RS256 signatures for service account sign-in; ring was already in the tree |
| Redis | none: a RESP2 client in `redis.rs` | |
| SQL parsing | sqlparser 0.63 (`visitor`) | Safe mode's read-only check, SQLite table rebuilds |
| TLS | rustls 0.23.46 (ring), native-tls, rustls-native-certs | Both rustls providers end up compiled in; `install_tls_provider()` picks ring |
| SSH | russh 0.64, russh-config | Tunnels; known_hosts accept-new |
| Secrets | keyring 3 (macOS Keychain, Secret Service), secrecy, argon2, aes-gcm, sha2, subtle | `SecretString` everywhere a password travels |
| MCP | rmcp 3.5.1 (server, streamable HTTP) | |
| Sandboxed JS | rquickjs 0.14 | Plugins and the mongosh shell |
| macOS auth | robius-authentication 0.3 | Touch ID for safe mode and the passcode |
| Errors | thiserror 2 | One `Error` enum |
| Tests | proptest (dev), cargo-fuzz target in `src-tauri/fuzz` | |

## UI

| Package | Version | Use |
| --- | --- | --- |
| codemirror and `@codemirror/*` | 6.x | SQL and JavaScript editors, find/replace, completion |
| `@replit/codemirror-vim` | 6.3.0, pinned | Vim key binding, loaded only when chosen |
| sql-formatter | 15.9.0, pinned | Format SQL (⌘I) |

Charts (plain SVG), the ER diagram's layout, the virtualized grid, fuzzy search and the JSON tree are hand-written, with no library (DEC-016).

## Tooling

| Tool | Where | Notes |
| --- | --- | --- |
| `npm run check` | `package.json` | tsc, node tests, rustfmt, Clippy with `-D warnings`, cargo test |
| cargo-deny, cargo-audit | `npm run audit`; `src-tauri/deny.toml`, `.cargo/audit.toml` | License allowlist, advisories, no unknown sources. One ignored advisory: RUSTSEC-2023-0071 (rsa via russh, no fix released) |
| GitHub Actions | `.github/workflows/check.yml` | `npm ci`, `npm run build`, `npm run check` on macOS for every push and PR |
| Docker Compose | `scripts/test-env.yml` via `scripts/test-env.sh` | Local test servers for every networked engine and an sshd (see [DEVELOPMENT.md](DEVELOPMENT.md)) |
| Bundling | `scripts/build.sh` (`npm run bundle`) | Checks, audit, the MCP bridge, then `tauri build`; signs updates when the key is present |

## Not used, on purpose

- No UI framework (React, Vue…). **Inferred** from the scaffold; never discussed.
- No Tailwind: asked about on 2026-10-17, advised against, not decided (DEC-020).
- No AWS SDK, no chart library, no diagram-layout library (DEC-016).
- `oracle-rs` (pure Rust): tried and dropped (DEC-009).
