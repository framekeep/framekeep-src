//! One face over two very different pipes.
//!
//! Windows gets a named pipe, macOS a Unix domain socket. Neither is TCP: a
//! localhost port is reachable by every process on the machine, a browser tab
//! included, while both of these carry an OS-level access rule.
//!
//! The address itself carries the protocol version -- `framekeep-v1` -- so a
//! future incompatible protocol listens somewhere else instead of two versions
//! meeting on one address and failing in the middle of a conversation.

#[cfg(windows)]
#[path = "transport_windows.rs"]
mod imp;

#[cfg(unix)]
#[path = "transport_unix.rs"]
mod imp;

pub use imp::{default_address, Connection, Listener};

use std::path::PathBuf;

/// Where the running server writes the address it bound.
///
/// The MCP adapter is Node, and Node cannot read this account's SID -- so on
/// Windows it cannot work out `\\.\pipe\framekeep-v1-<SID>` for itself. Rather
/// than have it shell out to `whoami` and parse localised output, the server
/// that already knows the answer writes it down.
///
/// It also buys the common case for free: no file means no app, which the
/// adapter learns without spending its 300 ms connect budget.
///
/// The file is a hint, not a credential. Anything running as this user could
/// rewrite it -- and could equally run `framekeep-core` directly, so nothing is
/// lost by trusting it. The adapter still checks the shape before connecting.
pub fn address_file() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".framekeep").join("ipc-address"))
}

/// Publish the address, and take it back down when the guard is dropped.
///
/// A file left behind by a crash costs the adapter one failed connect and then
/// standalone -- the same outcome as no file, one timeout later.
pub struct Published(Option<PathBuf>);

impl Published {
    /// Published nothing, and will clean up nothing.
    pub fn none() -> Published {
        Published(None)
    }

    pub fn write(address: &str) -> std::io::Result<Published> {
        let Some(path) = address_file() else {
            return Ok(Published(None));
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, address)?;
        Ok(Published(Some(path)))
    }
}

impl Drop for Published {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}
