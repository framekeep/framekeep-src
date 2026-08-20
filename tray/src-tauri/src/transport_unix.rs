//! Unix domain socket transport. The macOS half of S3.2.
//!
//! `~/.framekeep/run/framekeep-v1.sock`, with the directory at 0700. Not a
//! localhost TCP port: any process on the machine can reach a TCP port,
//! including a web page through a browser, while a socket file carries
//! filesystem permissions.
//!
//! Written against the plan and std's documented behaviour, and **not yet run
//! on a Mac** -- there is no macOS machine in this project yet (S7). Treat any
//! claim about it as unverified until it has been.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener as StdListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Connection = UnixStream;

fn run_dir() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Couldn't work out where your home folder is, so there's nowhere to put the socket.",
        )
    })?;
    Ok(PathBuf::from(home).join(".framekeep").join("run"))
}

pub fn default_address() -> io::Result<String> {
    Ok(run_dir()?
        .join(format!("framekeep-v{}.sock", crate::protocol::PROTOCOL))
        .to_string_lossy()
        .into_owned())
}

pub struct Listener {
    inner: StdListener,
    path: PathBuf,
    name: String,
    shutting_down: AtomicBool,
}

impl Listener {
    pub fn bind() -> io::Result<Listener> {
        Listener::bind_at(&default_address()?)
    }

    /// Ownership is expressed through file permissions here, not a DACL, so
    /// this side has nothing to derive: 0700 on the directory, 0600 on the
    /// socket.
    pub fn bind_at(name: &str) -> io::Result<Listener> {
        let path = PathBuf::from(name);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            // Owner only. The socket inherits reachability from its directory.
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }

        // A socket file left behind by a crash looks identical to one owned by
        // a running instance. The only way to tell them apart is to knock.
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "Framekeep is already running. Look for it in the menu bar.",
                    ))
                }
                Err(_) => {
                    std::fs::remove_file(&path)?;
                }
            }
        }

        let inner = StdListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

        Ok(Listener {
            inner,
            path,
            name: name.to_string(),
            shutting_down: AtomicBool::new(false),
        })
    }

    pub fn address(&self) -> &str {
        &self.name
    }

    pub fn accept(&self) -> io::Result<Option<Connection>> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let (stream, _) = self.inner.accept()?;
        if self.shutting_down.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(stream))
    }

    /// Wakes the blocking `accept` by connecting to ourselves, the same way the
    /// Windows side does.
    pub fn shutdown(&self) -> io::Result<()> {
        self.shutting_down.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        Ok(())
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Leaving the file behind would make the next start pay for a knock
        // that always fails.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_path() -> String {
        let dir = std::env::temp_dir().join(format!("framekeep-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!(
            "s{}.sock",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .to_string_lossy()
        .into_owned()
    }

    #[test]
    fn a_live_instance_blocks_a_second_one() {
        let path = unique_path();
        let _first = Listener::bind_at(&path).unwrap();
        // Not `expect_err`: it needs `Listener: Debug`, and deriving that to
        // satisfy a test would put the socket path into every `{:?}` of a live
        // listener. Matching says exactly the same thing and costs the shipped
        // type nothing.
        let err = match Listener::bind_at(&path) {
            Ok(_) => panic!("second bind must be refused"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("already running"), "{err}");
    }

    #[test]
    fn a_socket_left_by_a_crash_is_cleared_rather_than_inherited() {
        let path = unique_path();
        // What a crash actually leaves behind is the socket file with *nobody
        // holding the descriptor*. `mem::forget` on a live `Listener` does not
        // reproduce that: leaking it skips `Drop` -- which is the point -- but
        // the fd stays open, so the socket is still listening and `bind_at` is
        // correct to answer "already running". The first run of this test, the
        // day the Linux job finally compiled, failed on exactly that.
        //
        // Binding a raw listener and dropping it closes the descriptor, and std
        // deliberately does not unlink the file. That is the residue.
        {
            let doomed = StdListener::bind(&path).expect("bind the instance about to die");
            drop(doomed);
        }
        assert!(
            PathBuf::from(&path).exists(),
            "the socket file must outlive the process for this to be testing anything"
        );
        // Nobody is listening, so this must succeed rather than report a
        // running instance.
        let _second = Listener::bind_at(&path).expect("stale socket should be cleared");
    }

    #[test]
    fn the_handshake_works_over_a_real_socket() {
        use std::io::{BufRead, BufReader, Write};

        let path = unique_path();
        let listener = std::sync::Arc::new(Listener::bind_at(&path).unwrap());
        let server = {
            let listener = listener.clone();
            std::thread::spawn(move || {
                while let Ok(Some(conn)) = listener.accept() {
                    std::thread::spawn(move || {
                        let mut s =
                            crate::session::Session::new(Box::new(crate::session::NotBuiltYet));
                        let _ = s.serve(conn);
                    });
                }
            })
        };

        let client = UnixStream::connect(&path).unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        writer
            .write_all(
                b"{\"id\":\"0\",\"method\":\"hello\",\"params\":{\"client\":\"framekeep-mcp\",\"protocol\":1}}\n",
            )
            .unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["result"]["protocol"], 1, "{line}");

        drop(reader);
        drop(writer);
        listener.shutdown().unwrap();
        server.join().unwrap();
    }
}
