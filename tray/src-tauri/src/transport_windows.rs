//! Named pipe transport. This is the Windows half of S3.2.
//!
//! Four rules from the plan, all load-bearing, none optional:
//!
//! 1. the name carries the user's SID -- the pipe namespace is machine-wide
//!    with no per-session split, so two Windows users would collide on a
//!    static name
//! 2. an explicit DACL granting that SID alone, protected from inheritance
//! 3. `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance, so another
//!    process cannot squat the name and answer in our place
//! 4. `PIPE_REJECT_REMOTE_CLIENTS`, so the pipe is not reachable over SMB
//!
//! The S0.1 spike proved a packaged (MSIX) process can create this pipe in the
//! global namespace and an unpackaged process can connect to it. It took two
//! shortcuts that do not belong in shipping code, and both are paid back here:
//! the SID came from parsing `whoami` output, and only one client could ever
//! connect. See `docs/experiments/s0-msix-named-pipe-result.md`.
//!
//! Byte mode, not message mode. The protocol is line-framed JSON, so message
//! boundaries would be a second framing layer that has to agree with the first
//! -- and Node clients read these pipes as byte streams anyway.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FlushFileBuffers, ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

// Values Win32 defines but windows-sys does not re-export in these modules.
// Spelled out rather than guessed at from another crate's constant table.
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const SDDL_REVISION_1: u32 = 1;
const TOKEN_QUERY: u32 = 0x0008;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_BROKEN_PIPE: u32 = 109;
const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
const ERROR_NO_DATA: u32 = 232;
const ERROR_ACCESS_DENIED: u32 = 5;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

/// The current user's SID as a string, from the process token.
///
/// The spike read this out of `whoami /user /fo csv`, which is fine for a
/// question about MSIX and wrong for a product: it spawns a process, parses
/// localised CSV, and inherits whatever PATH resolution finds first.
pub fn current_user_sid() -> io::Result<String> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(last_error());
        }
        let _token = OwnedHandle(token);

        let mut needed: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        let err = GetLastError();
        if err != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::from_raw_os_error(err as i32));
        }

        // u64 elements so the buffer is 8-byte aligned: TOKEN_USER holds a
        // pointer, and reading it out of a misaligned Vec<u8> is undefined.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        if GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr() as *mut _,
            needed,
            &mut needed,
        ) == 0
        {
            return Err(last_error());
        }

        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut raw: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut raw) == 0 {
            return Err(last_error());
        }
        let mut len = 0;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
        LocalFree(raw as *mut _);
        Ok(sid)
    }
}

/// `\\.\pipe\framekeep-v1-<SID>`.
pub fn default_address() -> io::Result<String> {
    Ok(format!(
        r"\\.\pipe\framekeep-v{}-{}",
        crate::protocol::PROTOCOL,
        current_user_sid()?
    ))
}

/// A handle that closes itself. Every raw handle in this file is owned by one
/// of these, so no error path can leak one.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: a HANDLE is a process-wide token, not a pointer into this process's
// memory. Each OwnedHandle is the sole owner of its handle and is moved, never
// copied, so no two threads can close the same one.
unsafe impl Send for OwnedHandle {}

/// The security descriptor, built once and reused for every pipe instance.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0 as *mut _) };
        }
    }
}

// SAFETY: read-only after construction. It is only ever handed to
// CreateNamedPipeW, which does not modify it.
unsafe impl Send for Descriptor {}
unsafe impl Sync for Descriptor {}

struct Pending {
    /// The instance already created and waiting for its client. Keeping one of
    /// these alive at all times is what stops the name from lapsing between
    /// clients -- a lapse is exactly the window FILE_FLAG_FIRST_PIPE_INSTANCE
    /// exists to close.
    handle: Option<OwnedHandle>,
    first: bool,
}

pub struct Listener {
    name: String,
    wide_name: Vec<u16>,
    descriptor: Descriptor,
    pending: Mutex<Pending>,
    shutting_down: AtomicBool,
}

impl Listener {
    /// Bind the default address for this user.
    pub fn bind() -> io::Result<Listener> {
        Listener::bind_at(&default_address()?)
    }

    /// Bind a named address. Tests use this so each one owns a private name;
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` makes shared names mutually exclusive.
    ///
    /// The DACL owner is always this process's user and is not a parameter:
    /// a caller that could pass a different SID could widen the rule, and no
    /// caller has a reason to.
    pub fn bind_at(name: &str) -> io::Result<Listener> {
        let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(&sddl).as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::other(format!(
                "Framekeep could not build the access rule for its own pipe ({}). \
                 Report this with your Windows version.",
                last_error()
            )));
        }

        let listener = Listener {
            name: name.to_string(),
            wide_name: wide(name),
            descriptor: Descriptor(sd),
            pending: Mutex::new(Pending {
                handle: None,
                first: true,
            }),
            shutting_down: AtomicBool::new(false),
        };

        // Create the first instance now rather than on the first accept, so
        // "another copy is already running" is reported at startup instead of
        // whenever the first client happens to arrive.
        let mut guard = listener.pending.lock().unwrap();
        let handle = listener.create_instance(true)?;
        guard.handle = Some(handle);
        guard.first = false;
        drop(guard);

        Ok(listener)
    }

    pub fn address(&self) -> &str {
        &self.name
    }

    fn create_instance(&self, first: bool) -> io::Result<OwnedHandle> {
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: 0,
        };
        let handle = unsafe {
            CreateNamedPipeW(
                self.wide_name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &sa,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            let message = if code == ERROR_ACCESS_DENIED {
                "Framekeep is already running. Look for it in the system tray."
            } else {
                "Framekeep could not open its connection point. \
                 Restart the app; if it keeps happening, report it with this error."
            };
            return Err(io::Error::other(format!("{message} (pipe error {code})")));
        }
        Ok(OwnedHandle(handle))
    }

    /// Wait for one client. `Ok(None)` means [`Listener::shutdown`] was called.
    pub fn accept(&self) -> io::Result<Option<Connection>> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Ok(None);
        }

        let handle = {
            let mut guard = self.pending.lock().unwrap();
            match guard.handle.take() {
                Some(h) => h,
                None => {
                    // Only reached if preparing the next instance failed last
                    // time round. The error surfaces here rather than being
                    // swallowed at the point it happened.
                    let first = guard.first;
                    let h = self.create_instance(first)?;
                    guard.first = false;
                    h
                }
            }
        };

        let ok = unsafe { ConnectNamedPipe(handle.0, std::ptr::null_mut()) };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            // 535: the client won the race and was already connected when we
            // asked. That is a success.
            if code != ERROR_PIPE_CONNECTED {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }

        if self.shutting_down.load(Ordering::SeqCst) {
            return Ok(None);
        }

        // Hand the next instance to the namespace *before* handing this client
        // to the caller, so the name is never unowned.
        {
            let mut guard = self.pending.lock().unwrap();
            if guard.handle.is_none() {
                if let Ok(next) = self.create_instance(false) {
                    guard.handle = Some(next);
                }
            }
        }

        Ok(Some(Connection { handle }))
    }

    /// Stop the accept loop.
    ///
    /// `ConnectNamedPipe` is blocking, and cancelling it properly would mean
    /// overlapped I/O throughout. Connecting to our own pipe wakes it instead;
    /// the loop then sees the flag and returns `None`. The retries cover the
    /// case where shutdown lands while the loop is between instances.
    pub fn shutdown(&self) -> io::Result<()> {
        self.shutting_down.store(true, Ordering::SeqCst);
        for _ in 0..20 {
            let h = unsafe {
                CreateFileW(
                    self.wide_name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(h) };
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // The loop is not blocked on a connect, so it will see the flag on its
        // own at the top of the next accept.
        Ok(())
    }
}

pub struct Connection {
    handle: OwnedHandle,
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut got: u32 = 0;
        let len = buf.len().min(u32::MAX as usize) as u32;
        let ok = unsafe {
            ReadFile(
                self.handle.0,
                buf.as_mut_ptr(),
                len,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            // The peer hung up. That is end of stream, not a failure -- a
            // client closing its side is the normal way a session ends.
            if code == ERROR_BROKEN_PIPE || code == ERROR_PIPE_NOT_CONNECTED {
                return Ok(0);
            }
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(got as usize)
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut written: u32 = 0;
        let len = buf.len().min(u32::MAX as usize) as u32;
        let ok = unsafe {
            WriteFile(
                self.handle.0,
                buf.as_ptr(),
                len,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "The client disconnected before the answer was written.",
                ));
            }
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Best effort: the client may already be gone, and that is not an
        // error worth failing a finished exchange over.
        unsafe { FlushFileBuffers(self.handle.0) };
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            FlushFileBuffers(self.handle.0);
            DisconnectNamedPipe(self.handle.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn unique_name() -> String {
        format!(
            r"\\.\pipe\framekeep-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        )
    }

    /// Open the pipe the way any other program would.
    fn connect_as_client(name: &str) -> std::fs::File {
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(name)
            {
                Ok(f) => return f,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        panic!("could not connect to {name}");
    }

    #[test]
    fn the_sid_looks_like_a_sid() {
        let sid = current_user_sid().expect("reading the process token");
        assert!(sid.starts_with("S-1-"), "got {sid}");
        // Two reads must agree, or the pipe name is not stable across restarts.
        assert_eq!(sid, current_user_sid().unwrap());
    }

    #[test]
    fn the_default_address_carries_the_protocol_and_the_sid() {
        let addr = default_address().unwrap();
        assert!(
            addr.starts_with(r"\\.\pipe\framekeep-v1-S-1-"),
            "got {addr}"
        );
    }

    /// S0.3(a), re-proven against the real implementation rather than the spike.
    #[test]
    fn a_second_instance_cannot_take_the_same_name() {
        let name = unique_name();
        let _first = Listener::bind_at(&name).expect("first bind");
        let err = match Listener::bind_at(&name) {
            Ok(_) => panic!("a second bind on {name} must be refused"),
            Err(e) => e,
        };
        // And the message has to be the one a user can act on.
        assert!(
            err.to_string().contains("already running"),
            "unhelpful message: {err}"
        );
    }

    /// The whole slice over the real transport: pipe, handshake, boundary.
    #[test]
    fn a_client_can_handshake_and_is_still_refused_the_two_write_methods() {
        use std::io::{BufRead, BufReader};

        let name = unique_name();
        let listener = std::sync::Arc::new(Listener::bind_at(&name).unwrap());

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

        let client = connect_as_client(&name);
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let mut line = String::new();

        writer
            .write_all(
                b"{\"id\":\"0\",\"method\":\"hello\",\"params\":{\"client\":\"framekeep-mcp\",\"protocol\":1}}\n",
            )
            .unwrap();
        reader.read_line(&mut line).unwrap();
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["result"]["protocol"], 1, "{line}");

        line.clear();
        writer
            .write_all(b"{\"id\":\"1\",\"method\":\"video.ingest\",\"params\":{}}\n")
            .unwrap();
        reader.read_line(&mut line).unwrap();
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["error"]["code"], "FORBIDDEN", "{line}");

        drop(reader);
        drop(writer);
        listener.shutdown().unwrap();
        server.join().unwrap();
    }

    /// Cursor and Claude Code open at the same time -- the case the plan calls
    /// out by name.
    #[test]
    fn two_clients_are_served_at_once_not_one_after_the_other() {
        use std::io::{BufRead, BufReader};

        let name = unique_name();
        let listener = std::sync::Arc::new(Listener::bind_at(&name).unwrap());

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

        // Both connect before either says anything: a server that handled one
        // connection at a time would deadlock here rather than fail slowly.
        let a = connect_as_client(&name);
        let b = connect_as_client(&name);

        for (client, id) in [(a, "a"), (b, "b")] {
            let mut writer = client.try_clone().unwrap();
            let mut reader = BufReader::new(client);
            let mut line = String::new();
            writer
                .write_all(
                    format!(
                        "{{\"id\":\"{id}\",\"method\":\"hello\",\"params\":{{\"client\":\"framekeep-mcp\",\"protocol\":1}}}}\n"
                    )
                    .as_bytes(),
                )
                .unwrap();
            reader.read_line(&mut line).unwrap();
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(reply["id"], id, "{line}");
        }

        listener.shutdown().unwrap();
        server.join().unwrap();
    }
}
