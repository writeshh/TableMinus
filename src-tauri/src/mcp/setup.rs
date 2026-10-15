//! One-click setup: adds the `tableminus-mcp` bridge to an agent's MCP config file. The file is read first and
//! must be a JSON object (a file that doesn't parse is left alone, never rewritten); the original is copied aside
//! before the new one is written, and that write goes through a temporary file, so the agent's other servers and
//! settings survive whatever happens.

use super::write_private;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Agents that keep their MCP servers under `mcpServers` in a JSON file of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    ClaudeDesktop,
    Cursor,
    Windsurf,
}

/// Larger than any MCP config; a bigger file isn't one, and isn't read.
const MAX_CONFIG: u64 = 5 * 1024 * 1024;
/// The entry's name under `mcpServers`.
const NAME: &str = "tableminus";

impl Agent {
    /// Where the agent reads its MCP servers on this machine.
    pub fn config_path(self) -> Result<PathBuf> {
        let missing = || Error::Invalid("this machine has no home or config folder");
        Ok(match self {
            // ~/Library/Application Support on macOS, %APPDATA% on Windows, ~/.config on Linux.
            Agent::ClaudeDesktop => {
                dirs::config_dir().ok_or_else(missing)?.join("Claude").join("claude_desktop_config.json")
            }
            Agent::Cursor => dirs::home_dir().ok_or_else(missing)?.join(".cursor").join("mcp.json"),
            Agent::Windsurf => {
                dirs::home_dir().ok_or_else(missing)?.join(".codeium").join("windsurf").join("mcp_config.json")
            }
        })
    }
}

/// What the setup did: the file it wrote (or found already set up), and where the original went.
#[derive(Debug, Serialize, PartialEq)]
pub struct Installed {
    pub path: String,
    pub backup: Option<String>,
    /// The entry was there already, pointing at this bridge: nothing was written.
    pub already: bool,
}

/// Adds (or points at `bridge`) the `tableminus` entry of the MCP config at `path`. An existing entry keeps its other
/// keys, such as an `env` with a narrower token.
pub fn add_bridge(path: &Path, bridge: &str) -> Result<Installed> {
    let shown = path.display().to_string();
    let read_failed = |source| Error::File { path: shown.clone(), source };
    let original = match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_CONFIG => return Err(Error::Invalid("the agent's config file is too large to be one")),
        Ok(_) => Some(std::fs::read_to_string(path).map_err(read_failed)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(read_failed(e)),
    };
    let mut config: Map<String, Value> = match original.as_deref().map(str::trim) {
        None | Some("") => Map::new(),
        Some(text) => match serde_json::from_str(text) {
            Ok(Value::Object(o)) => o,
            _ => return Err(Error::Invalid("the agent's config file isn't a JSON object, so it was left as it is")),
        },
    };
    let servers = config.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(Error::Invalid("the agent's mcpServers isn't a JSON object, so the file was left as it is"));
    };
    let entry = servers.entry(NAME).or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(entry) = entry else {
        return Err(Error::Invalid("the agent's tableminus entry isn't a JSON object, so the file was left as it is"));
    };
    if entry.get("command").and_then(Value::as_str) == Some(bridge) {
        return Ok(Installed { path: shown, backup: None, already: true });
    }
    entry.insert("command".into(), Value::String(bridge.into()));
    let backup = match &original {
        Some(text) => {
            let mut to = path.as_os_str().to_owned();
            to.push(format!(
                ".tableminus-backup-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or_default()
            ));
            let to = PathBuf::from(to);
            write_private(&to, text.as_bytes())?;
            Some(to.display().to_string())
        }
        None => None,
    };
    // Printing a Value serde_json built itself doesn't fail; the message is for the impossible.
    let mut text = serde_json::to_string_pretty(&Value::Object(config))
        .map_err(|_| Error::Invalid("couldn't write the agent's config"))?;
    text.push('\n');
    write_private(path, text.as_bytes())?;
    Ok(Installed { path: shown, backup, already: false })
}

#[cfg(test)]
mod tests {
    use super::{Installed, add_bridge};
    use serde_json::{Value, json};
    use std::path::PathBuf;

    /// A fresh folder for one test's config file.
    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tableminus-setup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); // a leftover from an earlier run; none is fine
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn read(path: &std::path::Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn add_bridge_should_create_a_missing_config_without_a_backup() {
        let path = dir("new").join("sub").join("mcp.json");
        let done = add_bridge(&path, "/apps/tableminus-mcp").unwrap();
        assert_eq!(done, Installed { path: path.display().to_string(), backup: None, already: false });
        assert_eq!(read(&path), json!({ "mcpServers": { "tableminus": { "command": "/apps/tableminus-mcp" } } }));
    }

    #[test]
    fn add_bridge_should_keep_other_servers_and_settings_in_order_and_back_up_the_original() {
        let path = dir("keep").join("config.json");
        let original = r#"{"theme": "dark", "mcpServers": {"other": {"command": "x"}}, "alpha": 1}"#;
        std::fs::write(&path, original).unwrap();
        let done = add_bridge(&path, "/b").unwrap();
        let config = read(&path);
        assert_eq!(config["mcpServers"]["other"], json!({ "command": "x" }));
        assert_eq!(config["mcpServers"]["tableminus"], json!({ "command": "/b" }));
        let keys: Vec<_> = config.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["theme", "mcpServers", "alpha"], "the file's own order");
        assert_eq!(std::fs::read_to_string(done.backup.unwrap()).unwrap(), original);
    }

    #[test]
    fn add_bridge_should_leave_a_file_that_doesnt_parse_untouched() {
        let path = dir("broken").join("config.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(add_bridge(&path, "/b").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no backup either");
    }

    #[test]
    fn add_bridge_should_refuse_mcp_servers_that_isnt_an_object() {
        let path = dir("list").join("config.json");
        std::fs::write(&path, r#"{"mcpServers": []}"#).unwrap();
        assert!(add_bridge(&path, "/b").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"mcpServers": []}"#);
    }

    #[test]
    fn add_bridge_should_repoint_an_existing_entry_keeping_its_env() {
        let path = dir("env").join("config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"tableminus": {"command": "/old", "env": {"TABLEMINUS_MCP_TOKEN": "t"}}}}"#,
        )
        .unwrap();
        add_bridge(&path, "/new").unwrap();
        assert_eq!(
            read(&path)["mcpServers"]["tableminus"],
            json!({ "command": "/new", "env": { "TABLEMINUS_MCP_TOKEN": "t" } })
        );
    }

    #[test]
    fn add_bridge_should_write_nothing_when_already_set_up() {
        let path = dir("same").join("config.json");
        let text = r#"{"mcpServers": {"tableminus": {"command": "/b"}}}"#;
        std::fs::write(&path, text).unwrap();
        assert!(add_bridge(&path, "/b").unwrap().already);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}
