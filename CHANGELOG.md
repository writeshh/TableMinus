# Changelog

What changed in each version of TableMinus. The app shows a version's section when it offers the update, and
`scripts/latest-json.mjs` copies it into the release's `latest.json`: before a release, rename Unreleased to the
version in `src-tauri/tauri.conf.json`.

## 1.0.1

- Fixed: the structure editor refused to rename or move a MySQL column with an expression default such as
  `DEFAULT CURRENT_TIMESTAMP`, because it took the column for a generated one.

## 1.0.0

- New engines: Oracle (through Oracle Instant Client, which you install: tables, views, functions and procedures,
  structure editing, PL/SQL scripts, Explain), Google BigQuery (a service account's key; datasets, tables with
  edits, structure editing, scripts), MongoDB (collections, a document grid with edits, and a shell in mongosh syntax), Amazon DynamoDB
  (tables, items with edits, and PartiQL), Elasticsearch (indices, documents with edits, SQL and Dev Tools-style
  requests), Cassandra (keyspaces, rows as JSON with edits, and CQL), libSQL / Turso, and Cloudflare D1.
- In-app updates: TableMinus checks for a new version at startup (Settings › General can turn that off, or take
  beta versions too), shows what changed, and installs it on request. Help › Check for Updates… and Release Notes.
- Find Local Databases also finds Postgres sockets, MongoDB, and database servers on ports other than the usual.
  Postgres connections can use a Unix socket.
- SQL Server: named instances (`host\INSTANCE`); functions, procedures and sequences in the sidebar, with editors;
  column defaults, comments and triggers in the structure editor.
- Fixed: the filter and sort fields of MongoDB, DynamoDB, Elasticsearch and Cassandra tables, and the "Install from Git"
  fields for plugins and themes, took no typing; spaces typed in the structure editor were lost; Oracle connections
  could hang until macOS's local network permission was given; a picked row in a metrics table was unreadable.
- ClickHouse: `SET` and temporary tables last from one statement to the next; user-defined functions show in the
  sidebar, and open, save and drop there.
- Metrics boards: a data table's selected row feeds other widgets' queries (`order.id`), and a button can refresh
  just the widgets it names.
- ⌘T can open an empty tab that the next table or query fills (Settings › General).
- The AI assistant's HTTP proxy can take a user name and password.

## 0.1.0

The first version: PostgreSQL, MySQL, MariaDB, SQLite, Redis, CockroachDB, DuckDB, SQL Server and ClickHouse;
SSH tunnels and TLS; a data grid with filters, inline editing and row detail; the SQL editor with autocomplete,
formatting and parameters; structure editing and table operations; import, export, backup and restore; the ER
diagram, metrics boards, user management and the process list; safe mode levels and a passcode lock; an MCP server
and an AI assistant; plugins, custom scripts and themes.
