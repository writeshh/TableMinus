# Feature documents

Deep dives for features that cut across many files or carry safety rules. Most features don't need one: [../../TODO.md](../../TODO.md) describes each feature, its status and what was tried, and each module's top comment explains its code.

| Document | Covers |
| --- | --- |
| [safe-mode.md](safe-mode.md) | The five levels, the read-only check, where it applies, batch grants, the passcode lock |
| [engines.md](engines.md) | SQL engines on the `Driver` trait, the other engines, how to add one |
| [mcp-and-ai.md](mcp-and-ai.md) | The MCP server, the stdio bridge, tokens and approval; the AI assistant and ACP agents |

Add a document here when a feature spans several modules, has rules a future change could break, or needed a decision to build. Use this outline: purpose, status, behaviour, where the code is, data flow, edge cases, tests, open work. Don't create placeholders.
