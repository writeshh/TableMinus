# Plugins

**Purpose:** let a user add small actions to the grid's row menu and the sidebar's table menu, without a change to the app. A plugin is a JavaScript program that runs in a sandbox and can read the database only when the user allows it.

**Status:** done. One permission exists (`query`). One working example is in `examples/plugins/json-tools/`.

## What a plugin is

A plugin is a folder with two files:

- `plugin.json`: the manifest.
- A script, `main.js` by default.

The user installs the folder from a Git repository or from a local folder (Settings › Plugins). The app copies both files into `plugins/<id>/` in the config folder. On macOS this is `~/Library/Application Support/dev.ritesh.dbmanager/plugins/<id>/`.

## Write a plugin

### Manifest

`plugin.json` holds one JSON object. The app refuses a field that is not in this table.

| Field | Required | Rule |
| --- | --- | --- |
| `id` | yes | Up to 64 characters. Lowercase letters, digits and dashes. Starts with a letter or a digit. It becomes the folder name. |
| `name` | yes | 1 to 80 characters. |
| `version` | yes | Up to 40 characters. The app does not compare versions. |
| `description` | no | Up to 1000 characters. |
| `author` | no | Up to 200 characters. |
| `permissions` | no | A list of permissions the plugin asks for. Today the only value is `"query"`. |
| `main` | no | The script's file name. Default `main.js`. It must end in `.js`, sit next to `plugin.json` and have no path. |
| `actions` | yes | 1 to 50 actions. |

Each action has these fields, and no others:

| Field | Rule |
| --- | --- |
| `id` | Up to 64 characters. Letters, digits, `_`, `.` and `-`. Starts with a letter or a digit. Distinct in the plugin. |
| `title` | The text in the menu. 1 to 80 characters. |
| `on` | `"rows"` for the grid's row menu. `"table"` for the sidebar's table menu. |
| `run` | The name of a function in the script. It starts with a letter, `_` or `$`. |

Size limits: the manifest is at most 64 KB. The script is at most 1 MB.

### Script

Each action runs the function named in `run`. The function must be global. It gets one argument, `input`, and returns what the app does next.

Input for `"rows"` actions:

```js
{ table: "public.users" /* or null */, columns: ["id", "name"], rows: [{ id: "1", name: "Ada" }] }
```

Input for `"table"` actions:

```js
{ table: "public.users", engine: "postgres" /* or null */ }
```

Every cell value is a string or `null`. The app does not send numbers or booleans.

The function returns one of these:

| Return value | Effect |
| --- | --- |
| `{ clipboard: "text" }` | Copy the text. |
| `{ insert: "SQL" }` | Put the SQL in the editor. |
| `{ message: "text" }` | Show the text in the status line. |
| A string | Same as `{ message }`. |
| Nothing, or `null` | Show "<title>: done". |

An object can hold more than one of `clipboard`, `insert` and `message`. An object with any other key is an error. Each value must be a string or `null`.

A thrown error shows as `<title>: <error text>`.

### Reading the database

When the manifest asks for `query` and the user grants it, the script gets two globals:

- `canQuery`: `true` when `query` is usable.
- `query(sql)`: runs read-only SQL on the open connection and returns an array of row objects. Every value is a string or `null`. It returns at most 10,000 rows. It throws on an error.

`canQuery` is `false` in these cases. Check it before you call `query`:

- The user did not grant the permission.
- No connection is on screen.

Safe mode still applies. `query` accepts only SQL that the parser proves read-only, and the SQL runs inside the engine's read-only bounds. On a connection at Alert 1 or Safe 1, safe mode asks before every query. A plugin cannot answer that question, so `query` throws.

### The sandbox

Each action runs in a new QuickJS runtime. Nothing carries over between runs.

- No files, no network and no access to the rest of the app.
- 64 MB of memory, 1 MB of stack, 10 seconds of run time. The app stops a script that runs longer.
- The input and the result are at most 16 MB each.
- `console.log`, `console.warn` and `console.error` do nothing.
- The script runs in strict mode.

## Test a plugin

1. Open Settings › Plugins.
2. Choose "Install from a folder…" and pick the plugin folder.
3. Read the review dialog, tick the permissions and choose Install.
4. Open the menu that holds the action and run it.

The install step copies the files. After you edit the plugin, choose Update on its row. The app reads the folder again and asks for the permissions again. Ticked permissions stay ticked.

Test a Git install with the repository's `https://` address. The app reads the repository's default branch, and `plugin.json` must be at the top of the repository.

## Behaviour of the app

- **Install is two steps.** `plugin_fetch` reads and checks the plugin and holds it under a random token. The dialog shows the name, author, source, actions and each permission. `plugin_install` writes the plugin with the permissions the user ticked. The app refuses a permission that the manifest did not ask for.
- **Git source.** HTTPS only. No user name or password in the address. A shallow clone, with the same limits as themes (`app/themes.rs`). The app records the commit.
- **Update.** The app fetches the origin again and shows the same dialog. It then replaces the old folder only after the new files are complete.
- **Off switch.** A disabled plugin adds nothing to the menus and cannot run.
- **Broken plugin.** If the files on disk fail the checks, Settings › Plugins shows the reason. The menus skip the plugin.
- **Each run reads the files again.** `plugin_run` checks the plugin, the action and the grant every time.

## Where the code is

| Part | File |
| --- | --- |
| Manifest checks, install, sandbox, commands | `src-tauri/src/app/plugins.rs` |
| Settings › Plugins, the review dialog, menu items | `src/extend/plugins.ts` |
| Row menu input | `src/grid/gridview.ts` (`pluginMenu("rows", …)`) |
| Table menu input | `src/main.ts` (`pluginMenu("table", …)`) |
| Installed list and origins | `plugins.json` in the config folder (see [DATA-MODEL.md](../DATA-MODEL.md)) |
| Example | `examples/plugins/json-tools/` |

## Data flow

1. The user opens a menu. `pluginMenu` lists the enabled plugins' actions for that menu.
2. The user picks an action. The UI builds `input` and calls `plugin_run` with the plugin id, the action id and the on-screen connection's name.
3. The backend checks that the plugin is on, reads it, and finds the action.
4. If the grant includes `query` and a connection is on screen, the backend builds the `query` function. It goes through `guard` and `run_read_only` in `lib.rs`.
5. `run_script` runs the function in QuickJS. The result goes back as JSON.
6. `output` turns the JSON into `{ clipboard, insert, message }`. The UI copies, inserts or shows each part.

## Edge cases to keep

- Do not add a permission without a line in `PERMISSIONS` (`src/extend/plugins.ts`). The dialog shows that text to the user.
- Do not let a plugin write to the database. Safe mode's rules for `query` are in [safe-mode.md](safe-mode.md).
- Keep `deny_unknown_fields` on the manifest, the action and the output. A misspelled field then fails loudly.
- Keep the id and file name checks. The id becomes a folder name, and `plugin_remove` deletes that folder.
- Never put the user's grant in the manifest. The grant lives in `plugins.json`.

## Tests

Unit tests are at the end of `src-tauri/src/app/plugins.rs`. They cover the manifest checks, unknown fields and permissions, the function call, the time and memory limits, the missing `query` permission, rows from `query`, no file or network access, and the output format.

Run them with `cargo test plugins` in `src-tauri`.

## Open work

- `console` output is discarded, so a plugin author has only the thrown error to debug with.
- `query` is the only permission.
