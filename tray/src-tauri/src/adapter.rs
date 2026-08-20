//! The MCP adapter that ships inside the app, and how a client's Node reaches
//! it. S6.6.
//!
//! Until now the Setup screen asked the person to type `npx framekeep-mcp
//! init`. That was always marked temporary: `npx` needs npm, needs the network,
//! needs the right working folder, and on a machine without Node it fails with
//! `'npx' is not recognized` -- a sentence that points at the shim rather than
//! at Node. The adapter is about a megabyte of JavaScript we already build;
//! there is no reason to fetch it from a registry to reach the machine it is
//! already sitting on.
//!
//! Two decisions here are not obvious, and both were paid for:
//!
//! 1. **The adapter is copied to `~/.framekeep/mcp/` instead of being run from
//!    the package.** A packaged app lives under `C:\Program Files\WindowsApps`,
//!    whose ACLs are not ours, and the client's Node is a stranger process.
//!    S6.6 originally planned to test "can a client's Node read a file inside
//!    the package" -- but a loose layout registered for development runs
//!    straight out of the repo folder, so that test would have answered about
//!    the repo while looking like it had answered about WindowsApps. Copying
//!    sidesteps the question rather than betting on it. `~/.framekeep` is also
//!    the part of the profile MSIX does not virtualise, which is why the models
//!    already live there.
//!
//! 2. **Node still has to exist.** The config `init` writes names an absolute
//!    Node path (`process.execPath`) and an absolute script path, on purpose --
//!    that is what stops a client from spawning a bare `npx`. Bundling the
//!    adapter removes npm, the network and the typing; it does not remove Node,
//!    and pretending otherwise would move the same confusing failure one screen
//!    later. So Node is looked for first, and named plainly when it is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the adapter's entry point is called inside its folder.
const ENTRY: &str = "index.js";

/// The folder the adapter sits in, in a shipped layout: beside the executable,
/// the same shape `ffmpeg/` and `whisper/` already use.
const SHIPPED_DIR: &str = "mcp";

fn node_name() -> &'static str {
    if cfg!(windows) {
        "node.exe"
    } else {
        "node"
    }
}

/// Every place the shipped adapter might be, in the order they are tried.
///
/// Split out from `locate` for the same reason `core_candidates` is: the real
/// one reads `current_exe`, which under test is the test harness, so the case
/// that matters -- an installed app -- is unreachable without passing it in.
fn adapter_candidates(exe: Option<&Path>, env: Option<PathBuf>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(env) = env {
        candidates.push(env);
    }
    let Some(exe) = exe else {
        return candidates;
    };

    // Beside us: what an installed app looks like.
    if let Some(dir) = exe.parent() {
        candidates.push(dir.join(SHIPPED_DIR).join(ENTRY));
    }

    // tray/src-tauri/target/{debug,release}/x.exe -> repo root. Five steps up,
    // not four: `ancestors()` yields the path itself first. That off-by-one
    // already cost `core_candidates` a debugging session.
    if let Some(repo) = exe.ancestors().nth(5) {
        candidates.push(repo.join("mcp").join("dist").join(ENTRY));
    }
    candidates
}

/// The adapter as it ships. Not the copy a client runs -- see `install`.
pub fn locate() -> Result<PathBuf, String> {
    let candidates = adapter_candidates(
        std::env::current_exe().ok().as_deref(),
        std::env::var_os("FRAMEKEEP_MCP").map(PathBuf::from),
    );

    match candidates.iter().find(|c| c.is_file()) {
        Some(entry) => Ok(entry.clone()),
        None => Err(format!(
            "Couldn't find the Framekeep connector. Looked at: {}. \
             Build it with `npm run build` in mcp/, or set FRAMEKEEP_MCP.",
            candidates
                .iter()
                .map(|c| c.display().to_string())
                .collect::<Vec<_>>()
                .join(" · ")
        )),
    }
}

/// Where the copy a client runs lives. Beside the models, by the same rule:
/// rebuildable offline means the hidden folder, not the one a person browses.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".framekeep").join("mcp"))
}

/// Node, from PATH.
///
/// A plain PATH walk rather than letting `Command::new("node")` resolve it:
/// that would only fail at spawn time, with an `io::Error` that says a program
/// was not found without saying which program a person should go install.
pub fn node() -> Option<PathBuf> {
    let name = node_name();
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Put a runnable copy of the adapter where a client's Node can read it, and
/// answer with its entry point.
///
/// Stamped with the app version and skipped when the stamp matches, so pressing
/// Connect twice costs one file read. The stamp is written *last*: a copy
/// interrupted half way leaves no stamp and is redone, rather than leaving a
/// folder that claims to be a version it is not. Same discipline as core's
/// `.partial` model downloads.
pub fn install() -> Result<PathBuf, String> {
    let source = locate()?;
    let source_dir = source
        .parent()
        .ok_or_else(|| "The connector has no folder to copy from.".to_string())?;
    let dir = install_dir().ok_or_else(|| {
        "Couldn't work out your home folder, so there is nowhere to put the connector.".to_string()
    })?;

    let stamp = dir.join(".version");
    let want = env!("CARGO_PKG_VERSION");
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(want) {
        return Ok(dir.join(ENTRY));
    }

    // Replace rather than merge: a previous version's files are not ours to
    // reason about, and a stale module left beside a new one is the kind of
    // half-state that runs and misbehaves instead of failing.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("Couldn't clear {}: {e}", dir.display()))?;
    }
    copy_tree(source_dir, &dir)
        .map_err(|e| format!("Couldn't copy the connector to {}: {e}", dir.display()))?;
    std::fs::write(&stamp, want)
        .map_err(|e| format!("Couldn't finish installing the connector: {e}"))?;

    Ok(dir.join(ENTRY))
}

/// What happened, in the words the screen will show.
#[derive(Debug, serde::Serialize)]
pub struct Connected {
    /// The folder the config went into. With `--dir` init takes the folder
    /// as given rather than walking up to a repository, so this is exact --
    /// and it is the one fact the receipt line needs.
    pub folder: String,
    /// The adapter a client will now launch.
    pub entry: String,
    /// The Node that will launch it.
    pub node: String,
    /// `init`'s own account of which files it wrote, and where.
    pub report: String,
}

/// Write the MCP config into `folder` for every client that has one.
///
/// Runs the same `init` the npm package exposes -- one implementation, so the
/// button and the command line cannot drift into writing different configs.
pub fn connect(folder: &Path) -> Result<Connected, String> {
    let node = node().ok_or_else(|| {
        "Framekeep needs Node.js 20 or later to run the connector your AI client talks to. \
         Install it from nodejs.org, then try again."
            .to_string()
    })?;
    let entry = install()?;

    // Arguments stay an array all the way to the OS. `--dir` is passed rather
    // than relying on a working directory, because `init` walks up to the
    // nearest repository from wherever it starts: inheriting the app's own
    // folder would write the config somewhere no client looks, and report
    // "Wrote 3 files" while doing it.
    let mut cmd = Command::new(&node);
    cmd.arg(&entry).arg("init").arg("--dir").arg(folder);
    quiet(&mut cmd);

    let out = cmd
        .output()
        .map_err(|e| format!("Couldn't run the connector with {}: {e}", node.display()))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("The connector exited with {}.", out.status)
        } else {
            stderr.trim().to_string()
        });
    }

    Ok(Connected {
        folder: folder.display().to_string(),
        entry: entry.display().to_string(),
        node: node.display().to_string(),
        report: String::from_utf8_lossy(&out.stdout).trim().to_string(),
    })
}

/// No console window. A GUI process spawning a console program flashes one up,
/// and in a packaged build there is no console for it to inherit.
#[cfg(windows)]
fn quiet(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn quiet(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch that only exists in a package, and so is the one no dev run
    /// ever exercises. `core` shipped without its equivalent for a whole slice.
    #[test]
    fn an_installed_app_finds_the_adapter_beside_itself() {
        let dir = PathBuf::from("install").join("Framekeep_1.0.0.0_x64__abc");
        let found = adapter_candidates(Some(&dir.join("framekeep-tray.exe")), None);
        assert_eq!(
            found.first(),
            Some(&dir.join(SHIPPED_DIR).join(ENTRY)),
            "an installed app looks beside itself first: {found:?}"
        );
    }

    #[test]
    fn the_dev_tree_still_resolves_and_the_override_still_wins() {
        let repo = PathBuf::from("repo");
        let dev = repo
            .join("tray")
            .join("src-tauri")
            .join("target")
            .join("release")
            .join("framekeep-tray.exe");

        let found = adapter_candidates(Some(&dev), None);
        let expected = repo.join("mcp").join("dist").join(ENTRY);
        assert!(
            found.contains(&expected),
            "the dev layout was lost: {found:?}"
        );

        let mine = PathBuf::from("elsewhere").join(ENTRY);
        let overridden = adapter_candidates(Some(&dev), Some(mine.clone()));
        assert_eq!(overridden[0], mine, "FRAMEKEEP_MCP stopped winning");
    }

    /// The copy must land outside the package, not inside it. If this ever
    /// starts pointing at the install folder the whole reason for copying is
    /// gone, and nobody would notice until a client failed to read it.
    #[test]
    fn the_runnable_copy_lives_in_the_hidden_folder() {
        let Some(dir) = install_dir() else {
            return; // no home in this environment; nothing to assert
        };
        assert!(
            dir.ends_with(PathBuf::from(".framekeep").join("mcp")),
            "the connector copy moved out of ~/.framekeep: {}",
            dir.display()
        );
    }
}
