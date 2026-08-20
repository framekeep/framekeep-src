//! One connection, from `hello` to hang-up. This is S3.3 and the enforcement
//! half of S3.6.
//!
//! [`Session::handle_line`] takes bytes and returns a reply. No sockets, no
//! threads, no clock. That is deliberate: the whole protocol -- handshake,
//! version mismatch, the permission boundary, malformed input -- is exercised
//! by tests that never open a pipe, so the rules stay checked on every machine
//! including the ones with no named pipes at all.

use crate::method::{Caller, Method};
use crate::protocol::{read_line, ErrorCode, Line, Request, Response, PROTOCOL};
use serde::Deserialize;

/// What a session hands the rest of the app once a call is allowed through.
///
/// Everything below the boundary lives behind this trait, so the boundary can
/// be tested with a fake and the fake cannot accidentally be more permissive
/// than the real thing -- it is never consulted for a call that was refused.
pub trait Handlers {
    fn call(
        &mut self,
        method: Method,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, (ErrorCode, String)>;

    /// Advertised in the `hello` reply. Clients are required to read this
    /// rather than infer features from a version number, so a tray that has
    /// something switched off can say so.
    fn capabilities(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

/// Stand-in until S3.4 and S3.5 land. It refuses honestly instead of
/// pretending: a client that gets `NOT_READY` here knows the difference
/// between "this build cannot do it yet" and "your video is not ready".
pub struct NotBuiltYet;

impl Handlers for NotBuiltYet {
    fn call(
        &mut self,
        method: Method,
        _params: &serde_json::Value,
    ) -> Result<serde_json::Value, (ErrorCode, String)> {
        Err((
            ErrorCode::NotReady,
            format!(
                "This Framekeep build answers the handshake but not {} yet. \
                 Use the MCP server's standalone mode until the app is finished.",
                method.wire_name()
            ),
        ))
    }
}

/// `version` is deliberately absent: a client sends it, and this server has no
/// use for it. Splitting `protocol` from `version` is what lets the two sides
/// ship on their own schedules, so reading `version` to make a decision would
/// undo the reason the field exists. Unknown fields are ignored -- do not add
/// `deny_unknown_fields` here, it would break every future client that sends
/// one more thing.
#[derive(Debug, Deserialize)]
struct HelloParams {
    client: String,
    protocol: u32,
}

enum State {
    AwaitingHello,
    Ready { caller: Caller },
}

pub struct Session {
    state: State,
    handlers: Box<dyn Handlers + Send>,
}

impl Session {
    pub fn new(handlers: Box<dyn Handlers + Send>) -> Session {
        Session {
            state: State::AwaitingHello,
            handlers,
        }
    }

    /// Read requests and write replies until the peer goes away.
    ///
    /// The buffered reader owns the connection and lends it back through
    /// `get_mut` for each reply. The read borrow ends when `read_line` returns
    /// an owned line, so reading and writing never overlap and no part of this
    /// needs `unsafe`.
    pub fn serve(&mut self, conn: impl std::io::Read + std::io::Write) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut reader = std::io::BufReader::new(conn);
        loop {
            match read_line(&mut reader, &mut buf)? {
                Line::Eof => return Ok(()),
                Line::TooLong => {
                    // Answer, then hang up. Staying on the line would mean
                    // buffering the rest of whatever this is.
                    let out = Response::err(
                        serde_json::Value::Null,
                        ErrorCode::BadRequest,
                        format!(
                            "That message is longer than {} bytes. Frames travel as file paths here, never as data.",
                            crate::protocol::MAX_LINE_BYTES
                        ),
                    )
                    .to_line();
                    let conn = reader.get_mut();
                    let _ = conn.write_all(out.as_bytes());
                    let _ = conn.flush();
                    return Ok(());
                }
                Line::Message(line) => {
                    // A blank line is not an error worth answering; ignoring it
                    // keeps a client that pads its stream from getting a wall
                    // of BAD_REQUEST back.
                    if line.iter().all(|b| b.is_ascii_whitespace()) {
                        continue;
                    }
                    let out = self.handle_line(&line).to_line();
                    let conn = reader.get_mut();
                    conn.write_all(out.as_bytes())?;
                    conn.flush()?;
                }
            }
        }
    }

    /// One request in, one reply out. The order of the checks below is the
    /// design:
    ///
    /// 1. unparseable JSON -> `BAD_REQUEST`, and the connection stays open
    /// 2. unknown method    -> `NOT_FOUND`
    /// 3. `hello`           -> handshake, and only here does the caller get a name
    /// 4. no handshake yet  -> `BAD_REQUEST`
    /// 5. not allowed       -> `FORBIDDEN`, *before* any handler sees it
    /// 6. otherwise         -> the handler
    ///
    /// Step 5 must stay above step 6. A refused call that still reached its
    /// handler would leak behaviour through timing and through side effects,
    /// and a handler that is not written yet would answer `NOT_READY` -- which
    /// reads like "try again later" for a thing that must never happen.
    pub fn handle_line(&mut self, line: &[u8]) -> Response {
        let request: Request = match serde_json::from_slice(line) {
            Ok(r) => r,
            Err(e) => {
                return Response::err(
                    serde_json::Value::Null,
                    ErrorCode::BadRequest,
                    format!(
                        "That line is not a JSON object with a `method` field ({e}). \
                         One JSON message per line, ending with a newline."
                    ),
                )
            }
        };
        let id = request.id.clone();

        let method = match Method::parse(&request.method) {
            Some(m) => m,
            None => {
                return Response::err(
                    id,
                    ErrorCode::NotFound,
                    format!(
                        "No method called `{}`. This server speaks: {}.",
                        request.method,
                        KNOWN_METHODS.join(", ")
                    ),
                )
            }
        };

        if method == Method::Hello {
            return self.handshake(id, &request.params);
        }

        let caller = match self.state {
            State::AwaitingHello => {
                return Response::err(
                    id,
                    ErrorCode::BadRequest,
                    "Send `hello` first, with your client name and protocol version.",
                )
            }
            State::Ready { caller } => caller,
        };

        if !method.allows(caller) {
            return Response::err(id, ErrorCode::Forbidden, method.refusal_message());
        }

        match self.handlers.call(method, &request.params) {
            Ok(result) => Response::ok(id, result),
            Err((code, message)) => Response::err(id, code, message),
        }
    }

    fn handshake(&mut self, id: serde_json::Value, params: &serde_json::Value) -> Response {
        let hello: HelloParams = match serde_json::from_value(params.clone()) {
            Ok(h) => h,
            Err(e) => {
                return Response::err(
                    id,
                    ErrorCode::BadRequest,
                    format!(
                        "`hello` needs `client` and `protocol` ({e}). \
                         Example: {{\"id\":\"0\",\"method\":\"hello\",\"params\":{{\"client\":\"framekeep-mcp\",\"version\":\"1.0.0\",\"protocol\":{PROTOCOL}}}}}"
                    ),
                )
            }
        };

        if let State::Ready { .. } = self.state {
            return Response::err(
                id,
                ErrorCode::BadRequest,
                "This connection already said hello. Open a second connection instead.",
            );
        }

        // Protocol before client name: a peer from a future version has to
        // learn *why* it is being turned away, not be told its name is odd.
        if hello.protocol != PROTOCOL {
            return Response::err(
                id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "This Framekeep speaks protocol {PROTOCOL}, you asked for {}. \
                     Update whichever of the two is older; until then the MCP server works standalone.",
                    hello.protocol
                ),
            );
        }

        // A differing `version` with a matching `protocol` is fine, and that is
        // the entire reason the two fields are separate.
        let caller = Caller::from_client_name(&hello.client);
        self.state = State::Ready { caller };

        Response::ok(
            id,
            serde_json::json!({
                "server": "framekeep-tray",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL,
                "capabilities": self.handlers.capabilities(),
            }),
        )
    }
}

const KNOWN_METHODS: [&str; 8] = [
    "hello",
    "queue.list",
    "queue.get",
    "video.map",
    "video.frames",
    "video.status",
    "video.ingest",
    "redaction.apply",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Spy {
        calls: std::sync::Arc<std::sync::Mutex<Vec<Method>>>,
    }

    impl Handlers for Spy {
        fn call(
            &mut self,
            method: Method,
            _p: &serde_json::Value,
        ) -> Result<serde_json::Value, (ErrorCode, String)> {
            self.calls.lock().unwrap().push(method);
            Ok(serde_json::json!({"called": method.wire_name()}))
        }
        fn capabilities(&self) -> Vec<&'static str> {
            vec!["queue"]
        }
    }

    fn session() -> (Session, std::sync::Arc<std::sync::Mutex<Vec<Method>>>) {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let spy = Spy {
            calls: calls.clone(),
        };
        (Session::new(Box::new(spy)), calls)
    }

    fn send(s: &mut Session, line: &str) -> serde_json::Value {
        let reply = s.handle_line(line.as_bytes());
        serde_json::from_str(&reply.to_line()).unwrap()
    }

    fn hello(s: &mut Session, client: &str) -> serde_json::Value {
        send(
            s,
            &format!(
                r#"{{"id":"0","method":"hello","params":{{"client":"{client}","version":"9.9.9","protocol":1}}}}"#
            ),
        )
    }

    #[test]
    fn the_handshake_answers_with_capabilities_not_a_version_to_guess_from() {
        let (mut s, _) = session();
        let r = hello(&mut s, "framekeep-mcp");
        assert_eq!(r["result"]["protocol"], 1);
        assert_eq!(r["result"]["server"], "framekeep-tray");
        assert_eq!(r["result"]["capabilities"][0], "queue");
    }

    #[test]
    fn a_different_client_version_is_fine_a_different_protocol_is_not() {
        let (mut s, _) = session();
        let r = send(
            &mut s,
            r#"{"id":"0","method":"hello","params":{"client":"framekeep-mcp","version":"0.0.1","protocol":2}}"#,
        );
        assert_eq!(r["error"]["code"], "PROTOCOL_MISMATCH");
        // And it says which version it does speak, so the peer can decide.
        assert!(r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("protocol 1"));
    }

    /// The rule from AGENTS.md, exercised end to end rather than in the table.
    #[test]
    fn mcp_is_refused_ingest_and_redaction_and_the_handler_never_sees_them() {
        let (mut s, calls) = session();
        hello(&mut s, "framekeep-mcp");

        for method in ["video.ingest", "redaction.apply"] {
            let r = send(
                &mut s,
                &format!(r#"{{"id":"1","method":"{method}","params":{{}}}}"#),
            );
            assert_eq!(r["error"]["code"], "FORBIDDEN", "{method} was not refused");
            // Refusal tells the model to hand back to the person.
            assert!(r["error"]["message"].as_str().unwrap().contains("you"));
        }
        assert!(
            calls.lock().unwrap().is_empty(),
            "a refused call reached the handler: {:?}",
            calls.lock().unwrap()
        );
    }

    #[test]
    fn the_gui_may_do_what_the_model_may_not() {
        let (mut s, calls) = session();
        hello(&mut s, "framekeep-tray");
        let r = send(
            &mut s,
            r#"{"id":"1","method":"redaction.apply","params":{}}"#,
        );
        assert!(r["error"].is_null(), "{r}");
        assert_eq!(calls.lock().unwrap().as_slice(), [Method::RedactionApply]);
    }

    #[test]
    fn nothing_works_before_hello() {
        let (mut s, calls) = session();
        let r = send(&mut s, r#"{"id":"1","method":"queue.list","params":{}}"#);
        assert_eq!(r["error"]["code"], "BAD_REQUEST");
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_second_hello_is_refused_rather_than_changing_who_you_are() {
        // Otherwise a connection could introduce itself as the GUI after being
        // refused as the adapter.
        let (mut s, calls) = session();
        hello(&mut s, "framekeep-mcp");
        let r = hello(&mut s, "framekeep-tray");
        assert_eq!(r["error"]["code"], "BAD_REQUEST");

        let r = send(&mut s, r#"{"id":"2","method":"video.ingest","params":{}}"#);
        assert_eq!(r["error"]["code"], "FORBIDDEN");
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_input_is_answered_not_hung_up_on() {
        let (mut s, _) = session();
        let r = send(&mut s, "{not json");
        assert_eq!(r["error"]["code"], "BAD_REQUEST");
        // The connection is still usable: the handshake works right after.
        let r = hello(&mut s, "framekeep-mcp");
        assert!(r["error"].is_null());
    }

    #[test]
    fn an_unknown_method_says_what_this_server_does_speak() {
        let (mut s, _) = session();
        hello(&mut s, "framekeep-mcp");
        let r = send(&mut s, r#"{"id":"1","method":"video_map","params":{}}"#);
        assert_eq!(r["error"]["code"], "NOT_FOUND");
        assert!(r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("video.map"));
    }

    #[test]
    fn an_unknown_client_gets_a_handshake_and_then_nothing() {
        let (mut s, calls) = session();
        let r = hello(&mut s, "some-other-tool");
        assert!(r["error"].is_null(), "the handshake itself must succeed");
        let r = send(&mut s, r#"{"id":"1","method":"queue.list","params":{}}"#);
        assert_eq!(r["error"]["code"], "FORBIDDEN");
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn the_stand_in_handler_refuses_honestly() {
        let mut s = Session::new(Box::new(NotBuiltYet));
        hello(&mut s, "framekeep-mcp");
        let r = send(&mut s, r#"{"id":"1","method":"video.map","params":{}}"#);
        assert_eq!(r["error"]["code"], "NOT_READY");
        // But the boundary still holds in this build.
        let r = send(&mut s, r#"{"id":"2","method":"video.ingest","params":{}}"#);
        assert_eq!(r["error"]["code"], "FORBIDDEN");
    }

    #[test]
    fn a_full_conversation_over_a_pair_of_pipes() {
        // Exercises serve() itself: framing, replies in order, clean EOF.
        let input = concat!(
            r#"{"id":"0","method":"hello","params":{"client":"framekeep-mcp","protocol":1}}"#,
            "\n",
            r#"{"id":"1","method":"queue.list","params":{}}"#,
            "\n",
            r#"{"id":"2","method":"video.ingest","params":{}}"#,
            "\n",
        );
        struct Duplex {
            input: std::io::Cursor<Vec<u8>>,
            output: Vec<u8>,
        }
        impl std::io::Read for Duplex {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                std::io::Read::read(&mut self.input, b)
            }
        }
        impl std::io::Write for Duplex {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.output.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut duplex = Duplex {
            input: std::io::Cursor::new(input.as_bytes().to_vec()),
            output: Vec::new(),
        };
        let mut s = Session::new(Box::new(NotBuiltYet));
        s.serve(&mut duplex).unwrap();

        let lines: Vec<serde_json::Value> = String::from_utf8(duplex.output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3, "one reply per request");
        assert_eq!(lines[0]["id"], "0");
        assert_eq!(lines[0]["result"]["protocol"], 1);
        assert_eq!(lines[1]["error"]["code"], "NOT_READY");
        assert_eq!(lines[2]["error"]["code"], "FORBIDDEN");
    }
}
