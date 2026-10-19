# Engines

**Purpose:** one app over many databases, as TablePlus does (REQ-005). The per-engine status, what was tried and what's missing is in [../../TODO.md](../../TODO.md) §24. This page explains how engines are built, so a new one fits the pattern.

## Two kinds

**SQL engines** sit on the `Driver` trait (`src-tauri/src/sql/driver.rs`), held by `db::Conn`: Postgres (and CockroachDB), MySQL/MariaDB, SQLite/SQLCipher, DuckDB, SQL Server, ClickHouse, libSQL, D1, Oracle, BigQuery. They share the grid, filters, structure view and editor, DDL, export, import, the diagram, Copy Script As, Explain and safe mode. Engine differences are `match`es on `Dialect`.

**Other engines** have their own module, session type and commands: Redis (`nosql/redis.rs`, own view `src/nosql/redis.ts`), MongoDB, DynamoDB, Elasticsearch, Cassandra (each `src-tauri/src/<engine>/mod.rs`, shown in the shared document view `src/nosql/docview.ts` through an adapter `src/<engine>.ts`). Their sessions live in `sessions::Sessions<S>`.

## Adding a SQL engine

From TODO §24 and the commits that added DuckDB, SQL Server and ClickHouse (`7ab33eb`, `4a19f63`, `9622e14`):

1. `src-tauri/src/driver/<engine>.rs` implementing `Driver`: `batch`, `run` (every result set, cells as text), `execute_with`/`fetch_with` (bound parameters), `stream` (stops at the row limit), `ping`, `stopper` (how a query timeout cancels on the server), `lost_by` (which errors mean the connection dropped), `read_only_bounds`, and optionally `use_database`, `max_params`, `param`.
2. A `Dialect` variant in `sql/db.rs` (with `ident`, `table_ident`, `literal`, `param`), an `Engine` variant, and an arm in `Conn::open`.
3. Build: the compiler lists every `match` on `Dialect` that needs the engine's SQL (catalog, browse paging and `like_pattern`, structure, DDL, objects, diagram, processes, users, export, import…). Fill each in or return a clear "not supported" error.
4. `safety/readonly.rs`: the sqlparser dialect, and the engine's side-effect functions.
5. UI: the engine list (`ENGINES` and `FORM_SHAPES` in `src/connections/engines.ts`), the form's fields and defaults (every field optional, defaults as placeholders: REQ-021), URL schemes in `src/connections/connections.ts` and `src-tauri/Info.plist`, editor highlighting in `src/editor/editor.ts`, and the feature switches (backup tools, user management, Explain).
6. Discovery: a probe in `connect/discover.rs` if the server can be found locally.
7. A service in `scripts/test-env.yml` and ignored live tests that say what they need. Then a TODO.md entry saying what was tried, against which version, and what wasn't.

Postgres-compatible servers need none of that: they're a `flavor` on a Postgres connection (CockroachDB).

## Shared rules

- Cells are text end to end. Typed values that would lose precision as JavaScript numbers never reach the UI as numbers.
- Every connect has `CONNECT_TIMEOUT`; every query honours the query timeout through `Stopper`.
- Blocking client libraries run through `driver::blocking` (SQLite, DuckDB) or `spawn_blocking` with a channel (Oracle).
- TLS: map the connection's `Tls` modes faithfully (`connect/tls.rs`). When a library can't do a mode exactly (tiberius can't skip the name check for Verify CA; rustls can't skip it for MongoDB's), say so in TODO.md.
- If the server can't enforce read-only, safe mode rests on the parse. Write that down (TODO.md, ISSUE-010).

## Engine-specific choices worth knowing

- **ClickHouse** over HTTP, not the native protocol; one `session_id` per connection so `SET` and temporary tables persist. The grid is read-only (no row identity).
- **DuckDB:** a table without a primary key can't be edited, because DuckDB's rowid moves.
- **Oracle:** the `oracle` crate with Instant Client after `oracle-rs` failed (DEC-009). No autocommit on the wire: each statement commits unless the app opened a transaction. Schemas with their own objects act as "databases". PL/SQL blocks end at a `/` line.
- **BigQuery:** REST jobs, datasets as databases. Bound values are STRING parameters, cast in the SQL to their column's type (`bigquery::from_text`); a table's columns, keys and descriptions come from `tables.get`, since the emulator's INFORMATION_SCHEMA lacks most views. Transactions are queued and sent as one script.
- **SQL Server:** comments are `MS_Description` extended properties; a default is a named constraint, dropped by looking its name up before it changes or its column goes; triggers are AFTER triggers (`inserted`, `deleted`); routines save as `CREATE OR ALTER`.
- **Cassandra** rows go through `SELECT JSON` and `fromJson()`, so nothing decodes CQL types.
- **MongoDB** shell scripts run in QuickJS that only records operations; a script can't use one operation's result in the next (TODO §24).
- **D1** transactions are queued and sent as one batch.
