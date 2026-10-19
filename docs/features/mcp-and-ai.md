# MCP server and AI assistant

**Purpose:** let AI agents read the user's databases safely (MCP, TODO §20, like TablePlus's `tableplus-mcp`), and offer an in-app assistant that writes SQL with the schema as context (TODO §19).

**Status:** MCP done except remote access (deferred: REQ-R02). Assistant done for Anthropic, OpenAI-compatible endpoints and local ACP agents. GitHub Copilot isn't done. The proxy setting wasn't tried in the app window.

## MCP server (`src-tauri/src/mcp/`)

- Streamable HTTP on **127.0.0.1 only** (rmcp, stateless, no MCP sessions), on a preferred port with fallback to a free one.
- **Tools:** `list_connections` (never shows URLs), `list_tables`, `describe_table`, `run_query` (row limit and query timeout apply).
- **Tokens** (`mcp.json`, stored as SHA-256 hashes; each value shown once): named, scoped to some or all connections, read-only or read-write, revocable. A read-only token never writes and also runs its queries inside the driver's read-only bounds.
- **Approval:** a query that isn't provably read-only shows an Allow/Deny dialog in the app (on by default); a connection in safe mode always asks. Nobody answering in 60 s denies it.
- **The bridge** `tableminus-mcp` (`src/bin/tableminus-mcp.rs`): agents start it as a command; it forwards stdin JSON-RPC to the server using the port and token from `mcp-bridge.json` (mode 0600). `TABLEMINUS_MCP_TOKEN` gives it a narrower token. `npm run bundle` builds it and ships it next to the app's executable (`tauri.bundle.json` `externalBin`).
- **Setup:** Settings › MCP shows manual instructions, and can edit Claude Desktop, Cursor or Windsurf config files (`assistant/mcp/setup.rs`): it adds or repoints the `tableminus` entry, keeps everything else including key order and the entry's `env`, copies the original aside, writes through a temporary file, and leaves a file that isn't a JSON object untouched.
- **Activity log** and **connected clients** (per token since the app started). Disconnect revokes the token; the bridge's token is rotated.
- The passcode lock stops MCP requests too.

Tests: `assistant/mcp/tests.rs` runs a real server on a free port against SQLite (approvals, read-only tokens, safe-mode levels, the bridge file's permissions).

## AI assistant (`src-tauri/src/assistant/ai.rs`, `assistant/acp.rs`; UI `src/ai.ts`, `src/ai-text.ts`)

- Off until enabled under a consent notice that prompts go to the provider.
- **Providers:** Anthropic Messages API; OpenAI-compatible chat completions with a custom base URL (OpenAI, Ollama, LM Studio). HTTPS only except to this computer. Keys in the keychain. An optional HTTP proxy with Basic credentials (user in the address, password in the keychain).
- **Context:** the schema (tables, columns, types, foreign keys). One tool, `run_query`. The chat shows the SQL with Run and Don't run; it runs only when read-only, inside read-only bounds, at most 50 rows, and Safe-level password prompts still apply. SQL blocks have Insert and Copy.
- **Local agents over ACP:** Claude Code, Codex, Gemini CLI or any command, started without a shell. The agent reaches the open connection through the MCP bridge with a read-only token kept in memory while it runs. Its streamed chunks, tool calls and permission questions show in the chat; Stop cancels the turn; agents are killed with their chat or when the app quits. Tried against a fake agent and the real Claude Code adapter's `initialize`.
- Chats are saved per connection in `ai-chats.json`.

## Constraints

- The MCP server stays loopback-only until the owner decides otherwise (REQ-R02).
- Don't give an agent a read-write path by default. Read-only tokens are the default for ACP agents.
