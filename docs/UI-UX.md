# UI and UX

TableMinus copies TablePlus's **behaviour and layout**: the launcher, workspace tabs, sidebar, grid, editor, menus and shortcuts. It does **not** copy its visual design, which is the project's own brand. Requirements stated by the owner are REQ-020 to REQ-063 in [REQUIREMENTS.md](REQUIREMENTS.md).

## Visual language

Set by commit `36f0704` (2026-10-15, "Restyle the app in the brand palette"). The owner's brief for it isn't in the recovered session history; the commit message is the record:

- **Ink and paper grounds** (`--ink: #131313`, `--paper: #f5f5f1`) with **four primaries as flat fills carrying ink text**: blue `#0b87d1`, pink `#f4afc6`, orange `#eb5e12`, yellow `#fdd535`.
- **Environments map to the primaries:** local pink, development blue, staging yellow, production orange.
- **Shapes:** controls and rows are pills (`border-radius: 999px` on inputs, selects and buttons); windows and panels 16–20px radius; **the grid stays square**.
- **The launcher's side panel is ink in both schemes**, with a four-disc mark (`.mark` in `styles.css`). A blue mockingbird logo is wanted to replace it (REQ-063).
- **Tokens:** everything reads CSS variables on `:root` in `src/styles.css`, with `light-dark()` for both schemes: `--bg`, `--fg`, `--accent`, `--line`, `--hover`, `--warn`, `--danger`, `--ok`, the `--syn-*` editor colours and the `--grid-*` settings. Themes (`src/shell/theme-file.ts`, `src/shell/themes.ts`) override these variables.
- **Bundled themes:** TableMinus Light, TableMinus Dark, Paper, Harbor, Ember, Lagoon. Auto follows the system, with separate light and dark defaults. Users can edit, import, export and install themes from Git.
- **Type:** the system UI font; `--mono` (`ui-monospace`, SF Mono, Menlo, DejaVu Sans Mono) for the editor and grid. Font, size and padding can be set in a theme.

## Window and navigation

- **macOS title bar:** see-through overlay (`titleBarStyle: "Overlay"`, `hiddenTitle`, traffic lights at x 14, y 21 in `tauri.conf.json`). The workspace tab strip sits beside the traffic lights; its empty parts drag the window and double-click zooms. In full screen the strip starts at the left edge. The workspace's actions (Tools, Console, SQL, the safe-mode lock) and the connection pill (server : database : tab) sit at the right end of the bar. Windows and Linux keep their native frame.
- **Workspaces as tabs** across the top, like a browser: ⌘N or + opens a "New workspace" tab showing the launcher; connecting fills it.
- **Launcher:** searchable connection list, groups with icons, tags, status colours, Recent. Double-click connects (REQ-027).
- **Workspace:** left sidebar (Items / History / Search), item tabs (tables, SQL), optional split pane, row-detail sidebar (Space), assistant pane (⌥⌘4), console (⇧⌘C). Sidebar, editor/results split, console and row detail are resizable and remember their sizes (REQ-044).
- **Menu bar** mirrors TablePlus (File, Edit, View, Navigate, Connection, Tools, Window, Help), built from the command list. Items that don't apply are greyed out. Every command can be rebound in Settings › Shortcuts.

## States

- **Busy:** a pulsing dot on the status (`.status.busy`); long jobs get cards in one panel at the bottom right, with progress, Cancel and Show in Finder.
- **Lost connection:** a reconnect banner (⌘⇧R); a lost workspace shows a mark on its tab and in the switcher.
- **Safe mode:** a confirmation or password prompt in the app, never a silent refusal.
- **Errors:** the backend's message, shown as it is. Postgres errors show the server's own message, not "db error" (`a6558a8`).
- **Unverified work** is labelled in TODO.md rather than in the UI.

## Accessibility and input

- Pane dividers work from the keyboard (focus, arrows move 16px, Shift 64px, Home resets).
- Metrics widgets move and resize with arrow keys; charts have arrow-key tooltips.
- Icon buttons have instant tooltips (`data-tip`, `src/shell/tip.ts`), because the native `title` waits about a second and the tab strip clips it (REQ-061).
- A reduced-motion rule exists in `styles.css`, `metrics/charts.css` and `metrics/metrics.css`.

## The owner's feedback so far

Kept so the same corrections aren't needed twice. All of it is done unless marked otherwise.

- 2026-10-13: make the UI/UX better (the `/apple-design` skill); TablePlus-like launcher, engine picker, connection form and browsing workspace, from the owner's screenshots.
- 2026-10-15: shortcut hints too small; icons "super small and barely visible"; inputs inconsistent, so use `/design-taste-frontend` for one look everywhere; style the find/replace panel the same way; the sidebar's Items / History / Queries / Search segment is "too crammed" (Queries became a section of History); the title bar has "a lot of empty space", so use a custom one.
- 2026-10-17: a toolbar "looks super congested", so move items into the title bar; instant tooltips; the assistant icon goes at the end; a colour palette looked stretched; a + icon wasn't centred; more gap between the Name field and the icon grid; focus borders were clipped, and single click should show the same focus border; hover showed Edit/Delete on every saved item and didn't go away; widen a dialog to fit its items.

## Rejected or open

- **Manual drag-to-reorder of launcher connections:** built, then reverted at the owner's request within a minute (REQ-R01). Sorting by name, driver or tag stays.
- **Mockingbird logo:** one version shipped on 2026-10-15 and was reverted on 2026-10-16 (`0820765`, merged as PR #1). The reason isn't recorded; the item is open again (DEC-021).
- **Tailwind CSS:** advised against; the owner didn't decide (DEC-020).
