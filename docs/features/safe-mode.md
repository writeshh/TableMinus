# Safe mode

**Purpose:** keep the user from changing a database by accident: a write against production, a "read" that calls `nextval`, an AI agent's query. TablePlus's five levels, set per connection, enforced in Rust wherever SQL or a command runs. REQ-011, REQ-016.

**Status:** done for every engine (TODO §18, §26). Live-tested on Postgres, MySQL and SQLite; the other engines by their own unit and live tests.

## Levels (`src-tauri/src/safety.rs`)

| Level | Asks before | Wants |
| --- | --- | --- |
| Silent | nothing | |
| Alert 1 | every query | a click |
| Alert 2 (default) | anything not provably read-only | a click |
| Safe 1 | every query | the connection's database password, or Touch ID |
| Safe 2 | anything not provably read-only | the password, or Touch ID |

A default for new connections is in Settings › Security. Leaving a Safe level, from the toolbar lock or the connection form, wants the password too (`may_change_safety`).

## How a request is checked

1. **Is it read-only?** `readonly::is_read_only(engine, sql)` parses with sqlparser in the engine's dialect. Anything it can't prove read-only counts as a write. SELECTs calling a side-effect built-in (`nextval`, `pg_terminate_backend`, `lo_unlink`, `dblink`, advisory locks, MySQL `GET_LOCK`, `load_extension`…; the `SIDE_EFFECTS` list) and `SELECT … INTO` count as writes. Non-SQL engines classify their own commands: Redis by command name, MongoDB by the operations a shell script recorded (`$out`/`$merge` are writes), DynamoDB, Elasticsearch and Cassandra by statement or request type.
2. **Does the level ask?** `safety::check(level, read_only, confirmed, unlock, stored_password)`. It returns `ConfirmRequired` (code `confirm`) or `PasswordRequired` (code `password`) when the UI must ask.
3. **The UI asks and resends** (`src/safety.ts`, `guarded`): with `confirmed: true`, or an `Unlock` (`password`, `touch_id`, or a batch `grant`). The password is compared as SHA-256 digests in constant time.
4. **The database enforces it too** at Alert 2 and Safe 2: SQL the parse called read-only runs inside the driver's `read_only_bounds` (`begin read only`, `start transaction read only`, `pragma query_only = on`, ClickHouse `readonly=1`, Oracle `SET TRANSACTION READ ONLY`). A user-defined function that writes is then refused by the server and asked about like any write. SQL Server, libSQL and D1 can't do this, so they rest on the parse (ISSUE-010).

The connection's lock is released before the check, because Touch ID waits for the user (`lib.rs::guard`).

## Where it applies

The editor's `run_query`, grid saves (`rows.rs`), DDL previews run through `run_ddl`, imports (`jobs.rs`), restore (`backup.rs`, always confirmed), user management (`users.rs`), plugins' `query` permission (`plugins.rs`, read-only only), custom scripts (through `run_query`), metrics widgets (writes only from their own Run button, never on auto-refresh), the AI assistant (read-only only, at most 50 rows, after approval), and the MCP server (`mcp/mod.rs`: approval for unsafe queries, read-only tokens never write and also run inside read-only bounds). The non-SQL engines check in their own command modules.

## Batch grants

Run All at a Safe level asks once. `batch_grant` prompts for Touch ID and returns a token valid for 5 minutes; each statement sends it as `Unlock::Grant`. Tokens are kept as digests in memory only, so a restart ends them. A password needs no grant: resending it doesn't ask again.

## Related protections

- **Passcode lock** (`passcode.rs`): while locked, every database request is refused, MCP's included. The app starts locked until it knows whether a passcode is set.
- **Console log** masks password literals (`PASSWORD '…'`, `IDENTIFIED BY '…'`) before recording a statement (`console::redact`).
- **Imported connections** with startup scripts are held until the user sees the scripts (`launcher::Scripts`).
- **DDL undo note:** MySQL commits each DDL statement as it runs, so the previews say so, and a failure partway lists what already ran.

## Tests

`readonly.rs` unit tests and proptests (never panics; a write after any read is caught; SELECTs of quoted names pass), regression seeds in `src-tauri/proptest-regressions/`, a cargo-fuzz target in `src-tauri/fuzz/`, `lib.rs::guard_should_apply_the_open_connections_level`, and the MCP end-to-end tests in `mcp/tests.rs`.

## Edge cases to keep

- Postgres still lets a function write to temporary tables inside a read-only transaction (TODO §26).
- A Safe-level connection whose password mode is "Ask every time" has no saved password to check against (TODO §1).
- Don't move any of these checks into the UI only. The backend check is the guarantee, the UI is the prompt.
