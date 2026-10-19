# TableMinus documentation

How TableMinus is built, how to work on it and how its parts behave.

| Document | Covers |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | The shape of the app: the Rust folders, the UI folders, the backend boundary, sessions, a query's path, sandboxes |
| [DATA-MODEL.md](DATA-MODEL.md) | The files the app writes, the connection format and where each file lives |
| [TECH-STACK.md](TECH-STACK.md) | The languages, frameworks and crates, and what was left out on purpose |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Setup, commands, tests, test servers, releases and common failures |
| [CONVENTIONS.md](CONVENTIONS.md) | Coding rules, layout and the commit message format |
| [INTEGRATIONS.md](INTEGRATIONS.md) | Everything outside the app that it talks to: databases, GitHub Releases, AI providers |
| [UI-UX.md](UI-UX.md) | Behaviour and layout, the visual language, accessibility |
| [features/](features/README.md) | Deep dives: [safe mode](features/safe-mode.md), [engines](features/engines.md), [MCP and AI](features/mcp-and-ai.md) |

The user-facing release notes are in [../CHANGELOG.md](../CHANGELOG.md).
