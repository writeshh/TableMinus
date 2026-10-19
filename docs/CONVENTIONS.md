# Conventions

The rules for changing code here. The lints in `Cargo.toml` and `tsconfig.json` enforce what they can; the sections below record the rest, and the patterns the code follows consistently.

## Before every commit

Run `npm run check` (tsc, rustfmt, Clippy with warnings as errors, tests). It must pass.

## Commits

Commit messages follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/):

```text
<type>(<scope>): <summary>

<body: what changed and why, wrapped at 72 columns>

<footer: BREAKING CHANGE: ..., Closes #123>
```

- **Type** is one of `feat` (a new feature), `fix` (a bug fix), `docs`, `style` (formatting, no logic), `refactor` (no behaviour change), `perf`, `test`, `build` (build system, dependencies), `ci` and `chore`.
- **Scope** is optional and names the area: an engine (`redis`, `oracle`), a folder (`sql`, `grid`, `workspace`, `shell`) or a feature (`safety`, `mcp`).
- **Summary** is imperative ("add", not "added"), starts in lower case, has no final period and stays within 50 characters where it can, 72 at most.
- **Body** explains what changed and why, not how. Leave it out when the summary says it all.
- A breaking change has `!` after the type or scope, or a `BREAKING CHANGE:` footer.
- One logical change per commit. Squash fixups and reverts of your own unmerged work before opening a pull request.
- Link issues in the footer (`Closes #123`, `Refs #45`).

## Rust rules

Sources: [Apollo Rust best practices](https://github.com/apollographql/rust-best-practices), [Corgea Rust security](https://corgea.com/learn/rust-security-best-practices), [kobogithub rust-best-practices](https://github.com/kobogithub/knowledge/tree/prod/skills/rust-best-practices). The lints in `Cargo.toml` enforce what they can; the rest is on review.

**Lints**
- `unsafe_code` is forbidden. `clippy::all` is denied; `pedantic`, `unwrap_used`, `expect_used` and `panic` are enforced. Tests may unwrap (`clippy.toml`).
- Fix a warning rather than silencing it. When a lint is wrong for one spot, use `#[expect(lint, reason = "...")]` on that item, never a crate-wide `allow`.

**Errors**
- Every fallible function returns `crate::error::Result`. Add a variant to `error::Error` (thiserror) rather than passing strings around; it reaches the UI as its message.
- No `unwrap`/`expect`/`panic!` outside tests. Use `?`, `let ... else`, `ok_or`, `unwrap_or_default`.
- `let _ =` on a `Result` needs a comment saying why the error doesn't matter.
- Never lose user data on an error: refuse to write over a file that failed to load, and write files to a temporary path and then rename.

**Input and SQL**
- Values go into SQL as bound parameters (`$n` / `?`; `PgText` on Postgres). Only two things are escaped by hand: identifiers, through `Conn::ident` (`Dialect::ident`), and string literals in DDL, which takes no parameters (comments), through `Dialect::literal`.
- Fixed value sets are enums (`Engine`), not strings or booleans. Types the UI sends are `#[serde(deny_unknown_fields)]`.
- Cap anything untrusted that drives an allocation or a loop (row counts, file sizes, import lengths).

**Secrets**
- Passwords and URLs holding passwords are `secrecy::SecretString`. Call `expose_secret()` only at the point of use. Never `Debug`, log or persist them; the keychain is the only store (through `app/secrets.rs`, which falls back to its encrypted vault on Linux without a Secret Service).

**Async**
- Every network call has a timeout (`CONNECT_TIMEOUT` for connects).
- Never hold a `std::sync::Mutex` guard across `.await`. Blocking work (SQLite, file I/O on large files, external tools) belongs in `spawn_blocking`.
- External tools (`pg_dump` etc.): `Command::arg` per argument, never `sh -c`; passwords via environment variables, never arguments.

**Layout**
- A module with children is `name.rs` beside a `name/` folder, never `name/mod.rs`, so an editor's tabs show names, not a row of `mod.rs`. Group modules by domain in folders (`sql/`, `nosql/`, `connect/`…, see [ARCHITECTURE.md](ARCHITECTURE.md#the-rust-layout)); `lib.rs` keeps the commands and `error.rs` stays at the top.
- Unit tests sit in the file or in `name/tests.rs` beside it; there is no `tests/` folder, as the crate exposes no public API.
- Refer to a module by its full path (`crate::sql::db`), not through `lib.rs`'s `use` lines.

**Style**
- Borrow in signatures (`&str`, `&[T]`, `&Path`); clone only when ownership is really needed.
- Comments say why, not what. Doc comments (`///`) on items whose purpose isn't obvious from the name; `//!` at the top of each module.
- Extract a function when the name adds meaning or the same decision appears three times, not to save lines.
- Tests: one behavior per test, named for the unit and what it should do (`save_refuses_to_overwrite_an_unreadable_file`), covering error paths too. Keep each test's action and assertion inline.

**Release**
- `overflow-checks = true` and `panic = "unwind"` stay in the release profile.
- Before a release, and after adding or upgrading a dependency: `npm run audit` (`cargo deny check` and `cargo audit`; install them with `cargo install --locked cargo-deny cargo-audit`). An ignored advisory needs its reason in `src-tauri/deny.toml`.

## Rust patterns in use (inferred)

- **Errors the UI acts on** get a stable code in `Error::code` (`error.rs`), mirrored in the `Code` type in `src/ipc.ts`. Add to both together.
- **Every path that runs SQL checks safe mode in the backend**, never only in the UI ([features/safe-mode.md](features/safe-mode.md)). A new command that runs user SQL calls `readonly::is_read_only` and `safety::check`.
- **New SQL engines** go on the `Driver` trait with a `Dialect` variant; let the compiler list the `match`es to fill ([features/engines.md](features/engines.md)). Non-SQL engines get their own module and a `sessions::Sessions<S>`.
- **Statements are generated first, then run as shown:** DDL, row saves, user edits and object edits return SQL for the preview, and the run executes the same plan (`sql/ddl.rs`, `sql/rows.rs`, `sql/users.rs`).
- **Long work** runs on its own connection and reports through events (`transfer/jobs.rs`, `transfer/backup.rs`).
- **Ignored tests** say exactly what they need in the `#[ignore = "…"]` reason and clean up after themselves (scratch databases, specific Redis db numbers).
- `rustfmt.toml`: 120 columns, `use_small_heuristics = "Max"`: dense one-line expressions are the house style.
- Dependencies carry a comment in `Cargo.toml` saying why they're there, especially when "already in the tree through X".

## TypeScript and UI patterns (inferred)

- Modules live in a folder per area (`src/shell`, `workspace`, `grid`…, see [ARCHITECTURE.md](ARCHITECTURE.md#the-ui)); a module's test and stylesheet sit beside it. A new module goes in the folder of the feature it belongs to, and imports from another folder use its path (`../workspace/state`).
- Plain DOM, no framework: `el(tag, props, children)` and `$(id)` from `src/dom.ts`. Static structure in `index.html`.
- Call the backend only through `invoke` from `src/ipc.ts`, never `@tauri-apps/api/core` directly, and branch on `codeOf(e)`, not on message text. A raw `invoke` once made errors show as `[object Object]`.
- Keep logic that can be tested out of DOM code, in a `*-text.ts` or similar module with a `*.test.ts` beside it (`node:test`, `node:assert`).
- Commands (anything with a shortcut or a menu item) go in the `shell/commands.ts` registry, so the keyboard, the menu bar, Open Anything and Settings › Shortcuts stay in step.
- Settings go through `shell/prefs.ts` (`readPref`/`writePref`), not localStorage directly.
- Feature modules that add a Settings tab register it on import (`import "./mcp"` in `main.ts`). Import order in `main.ts` is load order, so keep side-effect imports where they are.
- CSS: design tokens on `:root` in `styles.css` (`--fg`, `--line`, `--accent`…, `light-dark()` for both schemes); per-feature stylesheets imported by their module. Themes override the tokens ([UI-UX.md](UI-UX.md)).
- The CSP has `script-src 'self'`: no inline scripts and no `eval` on the page. Custom scripts run in a Web Worker, where the CSP doesn't apply because Tauri sends it only with HTML.

## Writing style

Comments, docs and commit bodies are plain, concrete English: say what it does and what was tried. Be honest about what was tested and against what ("tried against SQL Server 2022; not in the app window"); never say something was tried when it wasn't.

## Dependencies

- Prefer no new dependency. The project has hand-written a RESP2 client, SigV4 signing, SVG charts and a diagram layout rather than add one.
- A new crate needs `npm run audit` to pass (license allowlist in `deny.toml`), and a comment in `Cargo.toml` saying why.
- Pin UI packages whose behaviour matters exactly (`sql-formatter` 15.9.0, `@replit/codemirror-vim` 6.3.0).
