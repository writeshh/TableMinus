# UI and UX

TableMinus copies TablePlus's **behaviour and layout**: the launcher, workspace tabs, sidebar, grid, editor, menus and shortcuts. It does **not** copy its visual design, which is the project's own brand.

## Visual language

The brand palette:

- **Ink and paper grounds** (`--ink: #131313`, `--paper: #f5f5f1`) with **four primaries as flat fills carrying ink text**: blue `#0b87d1`, pink `#f4afc6`, orange `#eb5e12`, yellow `#fdd535`.
- **Environments map to the primaries:** local pink, development blue, staging yellow, production orange.
- **Shapes:** controls and rows are pills (`border-radius: 999px` on inputs, selects and buttons); other corners use the `--radius-*` scale (2, 4, 6, 8, 12, 16px), with 16px on windows and panels; **the grid stays square**.
- **The launcher's side panel follows the theme**: the sidebar's ground (`--side`) in a light theme, ink in a dark one. Its mark is the bare blue mockingbird (`.mark` in `styles.css`, drawn from `src/mark.svg`). A dark theme shows `src/mark-dark.svg`, whose facets are lifted so the darkest still reads on ink (`themes.ts` sets `data-scheme` on the root for this). The lock screen shows the same mark. The app icon is the bird on a paper plate (`--paper`) with a soft shadow, following the macOS icon grid (`src-tauri/icons/source/icon.svg`). The icons are made from it with `npx tauri icon`.
- **Tokens:** everything reads CSS variables on `:root` in `src/styles.css`, with `light-dark()` for both schemes: `--bg`, `--fg`, `--accent`, `--line`, `--hover`, `--warn`, `--danger`, `--ok`, the `--syn-*` editor colours, the `--grid-*` settings, the `--text-*` sizes (10, 11, 12, 13, 15, 18 and 34px), the `--shadow-*` elevations (dialog, popover, tip), the `--control-*` heights (24, 28, 32 and 36px) and `--bar-h` (40px), the one height of the database tabs, the object tabs, the table header and the table footer. `src/styles.test.ts` fails on a raw px font size, radius, shadow or colour anywhere else. Themes (`src/shell/theme-file.ts`, `src/shell/themes.ts`) override these variables.
- **Bundled themes:** TableMinus Light, TableMinus Dark, Paper, Harbor, Ember, Lagoon. Auto follows the system, with separate light and dark defaults. Users can edit, import, export and install themes from Git.
- **Type:** the system UI font; `--mono` (`ui-monospace`, SF Mono, Menlo, DejaVu Sans Mono) for the editor and grid. Font, size and padding can be set in a theme.

## Window and navigation

- **macOS title bar:** see-through overlay (`titleBarStyle: "Overlay"`, `hiddenTitle`, traffic lights at x 14, y 21 in `tauri.conf.json`). The workspace tab strip sits beside the traffic lights; its empty parts drag the window and double-click zooms. In full screen the strip starts at the left edge. The workspace's actions sit at the right end of the bar: the connection pill (tag | server : tab), the database to switch, the tool icons, Tools, Console, SQL and the safe-mode lock. There is no header row under the bar. Windows and Linux keep their native frame.
- **Workspaces as tabs** across the top, like a browser: ⌘N or + opens a "New workspace" tab showing the launcher; connecting fills it.
- **Launcher:** searchable connection list, groups with icons, tags, status colours, Recent. Double-click connects.
- **Workspace:** left sidebar (Items / History / Search), item tabs (tables, SQL), optional split pane, row-detail sidebar (Space), assistant pane (⌥⌘4), console (⇧⌘C). Sidebar, editor/results split, console and row detail are resizable and remember their sizes.
- **Menu bar** mirrors TablePlus (File, Edit, View, Navigate, Connection, Tools, Window, Help), built from the command list. Items that don't apply are greyed out. Every command can be rebound in Settings › Shortcuts.

## States

- **Busy:** a pulsing dot on the status (`.status.busy`); long jobs get cards in one panel at the bottom right, with progress, Cancel and Show in Finder.
- **Lost connection:** a reconnect banner (⌘⇧R); a lost workspace shows a mark on its tab and in the switcher.
- **Safe mode:** a confirmation or password prompt in the app, never a silent refusal.
- **Errors:** the backend's message, shown as it is. Postgres errors show the server's own message, not "db error".

## Accessibility and input

- Pane dividers work from the keyboard (focus, arrows move 16px, Shift 64px, Home resets).
- Metrics widgets move and resize with arrow keys; charts have arrow-key tooltips.
- Icon buttons have instant tooltips (`data-tip`, `src/shell/tip.ts`), because the native `title` waits about a second and the tab strip clips it.
- A reduced-motion rule exists in `styles.css`, `metrics/charts.css` and `metrics/metrics.css`.

## Rejected or open

- **Manual drag-to-reorder of launcher connections:** built, then reverted. Sorting by name, driver or tag stays.
- **Mockingbird logo:** a first version was tried and reverted. The second, a faceted bird in profile, is on trial. If it is reverted, the four-disc mark comes back.
- **Tailwind CSS:** not used; the UI is hand-written CSS with design tokens.
