# Changelog

What changed in each version of TableMinus. The app shows a version's section when it offers the update, and
`scripts/latest-json.mjs` copies it into the release's `latest.json`: before a release, rename Unreleased to the
version in `src-tauri/tauri.conf.json`.

## 1.2.0

- New: a new logo and app icon, a blue mockingbird. In the Dock it sits on a dark plate with a light edge.
- New: notices about an action (saved, copied, could not export) show as a toast at the bottom right, also above
  an open window. Copies and settings that failed without a word now say why.
- New: menus show a tick beside the chosen item and open submenus to the side. The arrow keys move through a menu.
- Changed: text, secondary text and field outlines have more contrast. All text meets WCAG AA (4.5:1).
- Changed: the launcher's left panel and its mark follow the light or dark theme.
- Changed: every window has one title style, with Cancel and the main action at the right of its footer. A confirm
  window shows its question as its title, and its button is orange only when the step loses data.
- Changed: each question that asks for one value (a name, a password) uses the same small window.
- Changed: Settings keeps one height on every tab.
- Changed: the SQL run bar stays on one line. In a narrow window, Format, Explain and Export move into a More menu.
- Changed: every bar in the window has the same height.
- Changed: scroll bars are thin and show only while the pointer is over the part that scrolls. The setting "Hide
  scroll bars until the pointer is over the grid" is gone, because every scroll bar now works that way.
- Changed: windows and Metrics boards open below the title bar, clear of the window buttons.
- Changed: the sidebar shows a drawn glyph for each kind of object, and MongoDB collections have one too. The
  MongoDB and Redis sidebars match the SQL sidebar.
- Changed: text sizes and rounded corners come from one small set, so screens match. Some labels and
  corners are slightly larger or smaller.
- Changed: the row under the workspace tabs is gone. The database to switch and the file and row-detail
  buttons are now in the top bar, next to the connection pill. The data starts 40px higher.
- Changed: a table has a header with its name, row and column counts, Data and Structure, + Row,
  Filters, Columns and Export. The footer keeps only messages, unsaved edits and the pages.
- Fixed: opening a MongoDB database said "29 tables". It now names only the database.
- Fixed: the Structure view showed page buttons that did nothing.
- Fixed: the Jobs button opened a window named Notifications.

## 1.1.0

- New: Settings has a search box. Type a word to see the matching settings from every tab at once.
- New: Settings › General › Recent folders turns the Recent folder on or off in the Items pane and in the
  launcher. The right-click menu of the Items pane changes the same setting.
- Fixed: the tab close buttons, the table chevrons and the filter row buttons were slightly off centre. They are now
  drawn icons. The filter row's Apply-only button is larger.
- Changed: Settings › Shortcuts shows each key on its own keycap, and the tooltip names the keys in words.

## 1.0.2

- Security: updated the TLS certificate checks, the bundled root certificates, tokio and two cryptography crates to
  versions that fix six published advisories (RUSTSEC-2026-0381, -0382, -0383, -0385, -0386 and -0387).
- Fixed: the start screen said "Version 0.1.0" in every release. It now shows the installed version.

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
