//! Themes from a Git repository (Settings › Appearance › Install from Git…): a shallow clone over HTTPS into a
//! temporary folder, from which the theme files (`*.json` at the top and in `themes/`) are read and handed to the UI,
//! which checks each one as it does an imported file (theme-file.ts). The clone is removed afterwards. Updating runs
//! the same fetch again.

use crate::error::{Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

/// A theme file is small (theme-file.ts allows 64 KB).
const MAX_FILE: u64 = 64 * 1024;
/// More than any theme repository holds; a repository with more isn't read past this.
const MAX_FILES: usize = 100;
/// A shallow clone of a theme repository takes seconds.
const CLONE_TIMEOUT: Duration = Duration::from_secs(120);
/// Where theme files are looked for, relative to the repository's top.
const FOLDERS: [&str; 2] = ["", "themes"];

#[derive(Debug, Serialize, PartialEq)]
pub struct ThemeFile {
    /// Relative to the repository's top, with `/`.
    pub path: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct Fetched {
    /// The commit the files came from.
    pub commit: String,
    pub files: Vec<ThemeFile>,
}

/// Only `https://` URLs without credentials: git's other transports can run commands (`ext::`) or reach the disk.
pub(crate) fn checked_url(url: &str) -> Result<Url> {
    let u = Url::parse(url.trim())?;
    if u.scheme() != "https" {
        return Err(Error::Invalid("use the repository's https:// address"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(Error::Invalid("the repository address can't hold a user name or password"));
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err(Error::Invalid("the repository address has no host"));
    }
    Ok(u)
}

/// The theme files in `dir`: `*.json` in [`FOLDERS`], sorted by path. Links aren't followed, so a repository can't
/// point at files elsewhere on this computer; files over [`MAX_FILE`] are skipped.
fn collect(dir: &Path) -> Result<Vec<ThemeFile>> {
    let mut files = Vec::new();
    for folder in FOLDERS {
        let here = dir.join(folder);
        let Ok(entries) = std::fs::read_dir(&here) else { continue };
        for entry in entries {
            let entry = entry?;
            let meta = std::fs::symlink_metadata(entry.path())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !meta.is_file() || meta.len() > MAX_FILE || !name.to_ascii_lowercase().ends_with(".json") {
                continue;
            }
            if files.len() == MAX_FILES {
                break;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue }; // not UTF-8: no theme
            let path = if folder.is_empty() { name } else { format!("{folder}/{name}") };
            files.push(ThemeFile { path, text });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// A temporary folder, removed when dropped.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: the folder is in the system's temporary folder, which is cleared anyway.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs git with `args` and returns its standard output. Nothing interactive: no password prompts, no LFS downloads,
/// and only the `protocol` transport.
async fn git(args: &[&std::ffi::OsStr], protocol: &str) -> Result<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-c")
        .arg("protocol.allow=never")
        .arg("-c")
        .arg(format!("protocol.{protocol}.allow=always"))
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .kill_on_drop(true);
    let out = tokio::time::timeout(CLONE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| Error::Invalid("git took longer than two minutes; try again later"))?
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                Error::Invalid("git isn't installed; install it to add themes from a repository")
            }
            _ => Error::Io(e),
        })?;
    if !out.status.success() {
        let message = String::from_utf8_lossy(&out.stderr).trim().chars().take(400).collect::<String>();
        return Err(Error::Git(message));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Clones `url` (already checked) shallowly into `into`, over `protocol` only; returns the commit.
pub(crate) async fn shallow_clone(url: &str, protocol: &str, into: &Path) -> Result<String> {
    let clone = ["clone", "--depth", "1", "--single-branch", "--no-tags", "--quiet", "--"].map(std::ffi::OsStr::new);
    git(&[&clone[..], &[std::ffi::OsStr::new(url), into.as_os_str()]].concat(), protocol).await?;
    git(&["-C", &into.to_string_lossy(), "rev-parse", "HEAD"].map(std::ffi::OsStr::new), protocol).await
}

/// A fresh, empty temporary folder, removed when dropped.
pub(crate) fn scratch(what: &str) -> Result<Scratch> {
    Ok(Scratch(std::env::temp_dir().join(format!("tableminus-{what}-{}", crate::assistant::mcp::random_hex(8)?))))
}

/// Clones `url` (already checked) shallowly and reads its theme files.
async fn fetch(url: &str, protocol: &str) -> Result<Fetched> {
    let scratch = scratch("theme")?;
    let commit = shallow_clone(url, protocol, &scratch.0).await?;
    let path = scratch.0.clone();
    let files = tauri::async_runtime::spawn_blocking(move || collect(&path)).await??;
    if files.is_empty() {
        return Err(Error::Invalid("the repository has no .json files at its top or in themes/"));
    }
    Ok(Fetched { commit, files })
}

#[tauri::command]
pub async fn theme_git_fetch(url: String) -> Result<Fetched> {
    let url = checked_url(&url)?;
    fetch(url.as_str(), "https").await
}

#[cfg(test)]
mod tests {
    use super::{ThemeFile, checked_url, collect, fetch};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tableminus-themes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); // a leftover from an earlier run; none is fine
        std::fs::create_dir_all(d.join("themes")).unwrap();
        d
    }

    #[test]
    fn checked_url_should_take_only_https_without_credentials() {
        assert!(checked_url("https://github.com/someone/themes").is_ok());
        assert!(checked_url("http://github.com/someone/themes").is_err());
        assert!(checked_url("ext::sh -c touch% /tmp/pwned").is_err());
        assert!(checked_url("file:///etc").is_err());
        assert!(checked_url("https://user:pw@github.com/x/y").is_err());
    }

    #[test]
    fn collect_should_read_json_at_the_top_and_in_themes_only() {
        let d = dir("collect");
        std::fs::write(d.join("a.json"), "{}").unwrap();
        std::fs::write(d.join("themes").join("b.JSON"), "[]").unwrap();
        std::fs::write(d.join("readme.md"), "#").unwrap();
        std::fs::create_dir_all(d.join("other")).unwrap();
        std::fs::write(d.join("other").join("c.json"), "{}").unwrap();
        assert_eq!(
            collect(&d).unwrap(),
            [
                ThemeFile { path: "a.json".into(), text: "{}".into() },
                ThemeFile { path: "themes/b.JSON".into(), text: "[]".into() }
            ]
        );
    }

    #[test]
    fn collect_should_skip_large_files() {
        let d = dir("large");
        std::fs::write(d.join("big.json"), vec![b' '; 70 * 1024]).unwrap();
        assert_eq!(collect(&d).unwrap(), []);
    }

    #[cfg(unix)]
    #[test]
    fn collect_should_not_follow_links() {
        let d = dir("links");
        let outside = d.with_extension("secret.json");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, d.join("link.json")).unwrap();
        assert_eq!(collect(&d).unwrap(), []);
    }

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    /// Clones a local repository over the `file` transport, which the app itself never allows.
    #[test]
    fn fetch_should_read_a_repositorys_themes_at_its_commit() {
        let repo = dir("repo");
        run(&repo, &["init", "--quiet"]);
        std::fs::write(repo.join("themes").join("t.json"), r#"{"name":"T","scheme":"dark","vars":{}}"#).unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "t"]);
        let url = format!("file://{}", repo.display());
        let got = tauri::async_runtime::block_on(fetch(&url, "file")).unwrap();
        assert_eq!(got.commit.len(), 40);
        assert_eq!(got.files.len(), 1);
        assert_eq!(got.files[0].path, "themes/t.json");
    }

    #[test]
    fn fetch_should_refuse_a_transport_it_wasnt_given() {
        let repo = dir("refused");
        run(&repo, &["init", "--quiet"]);
        let url = format!("file://{}", repo.display());
        assert!(tauri::async_runtime::block_on(fetch(&url, "https")).is_err());
    }
}
