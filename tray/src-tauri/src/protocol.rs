//! The wire format: JSON Lines, one message per line, terminated by `\n`.
//!
//! Not full JSON-RPC. Four fields carry everything: `id`, `method`, `params`
//! on the way in; `id` plus one of `result` / `error` on the way out.
//!
//! `id` is opaque to this server. It is echoed back exactly as it arrived, so a
//! client that numbers its requests with strings, integers or anything else
//! keeps working without a change here.

use serde::{Deserialize, Serialize};
use std::io::BufRead;

/// Protocol version. It is also the `v1` in the pipe name, so a breaking change
/// gets a new address rather than two incompatible peers crashing into each
/// other on the old one.
pub const PROTOCOL: u32 = 1;

/// A single line may not exceed this. Frames never travel through the socket --
/// only paths do -- so nothing legitimate comes close.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct Request {
    /// Missing `id` parses as `null` and is echoed as `null`. A client that
    /// forgets it still gets an answer it can correlate by order.
    #[serde(default)]
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// English, and it has to say what broke and what to do next. This string
    /// reaches a human through the MCP adapter, so `Permission denied` is not
    /// an acceptable value here.
    pub message: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    ProtocolMismatch,
    Forbidden,
    AwaitingReview,
    NotFound,
    Gone,
    NotReady,
    BadRequest,
    CoreFailed,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ProtocolMismatch => "PROTOCOL_MISMATCH",
            ErrorCode::Forbidden => "FORBIDDEN",
            ErrorCode::AwaitingReview => "AWAITING_REVIEW",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::Gone => "GONE",
            ErrorCode::NotReady => "NOT_READY",
            ErrorCode::BadRequest => "BAD_REQUEST",
            ErrorCode::CoreFailed => "CORE_FAILED",
        }
    }
}

impl Response {
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Response {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: serde_json::Value, code: ErrorCode, message: impl Into<String>) -> Self {
        Response {
            id,
            result: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }

    /// Serialised with its trailing newline, ready to write.
    ///
    /// Serialisation of this type cannot realistically fail, but a panic here
    /// would take down a connection mid-answer, so the impossible branch still
    /// produces a line the client can parse and act on.
    pub fn to_line(&self) -> String {
        match serde_json::to_string(self) {
            Ok(s) => {
                let mut s = s;
                s.push('\n');
                s
            }
            Err(e) => format!(
                r#"{{"id":null,"error":{{"code":"CORE_FAILED","message":"Framekeep could not encode its own reply ({}). Restart the app; if it keeps happening, report it."}}}}{}"#,
                e.to_string().replace('"', "'"),
                '\n'
            ),
        }
    }
}

/// What one read produced.
pub enum Line {
    Message(Vec<u8>),
    /// Past [`MAX_LINE_BYTES`] with no newline in sight.
    TooLong,
    Eof,
}

/// Read one `\n`-terminated line, refusing to buffer without bound.
///
/// `BufRead::read_until` would be shorter, but it grows the buffer to whatever
/// the peer sends before returning -- so the cap could only be applied after
/// the memory was already gone. This checks as it goes.
///
/// A trailing `\r` is dropped, so a client writing CRLF is not punished for it.
pub fn read_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    buf.clear();
    loop {
        let (eof, found_newline, consumed) = {
            let available = match reader.fill_buf() {
                Ok(b) => b,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                (true, false, 0)
            } else {
                match available.iter().position(|&b| b == b'\n') {
                    Some(i) => {
                        buf.extend_from_slice(&available[..i]);
                        (false, true, i + 1)
                    }
                    None => {
                        let n = available.len();
                        buf.extend_from_slice(available);
                        (false, false, n)
                    }
                }
            }
        };
        reader.consume(consumed);

        if eof {
            return Ok(if buf.is_empty() {
                Line::Eof
            } else {
                // A last line with no newline is still a line.
                Line::Message(std::mem::take(buf))
            });
        }
        if found_newline {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(Line::Message(std::mem::take(buf)));
        }
        if buf.len() > MAX_LINE_BYTES {
            return Ok(Line::TooLong);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_serialise_as_the_names_the_plan_uses() {
        let r = Response::err(serde_json::json!("7"), ErrorCode::AwaitingReview, "x");
        let line = r.to_line();
        assert!(line.contains(r#""code":"AWAITING_REVIEW""#), "{line}");
        assert!(line.ends_with('\n'));
        // A successful reply must not carry an empty `error` key, and vice
        // versa: a client checking for the key's presence has to be right.
        assert!(!line.contains("\"result\""));
    }

    #[test]
    fn the_id_comes_back_exactly_as_it_went_in() {
        for id in [
            serde_json::json!("abc"),
            serde_json::json!(42),
            serde_json::json!(null),
        ] {
            let line = Response::ok(id.clone(), serde_json::json!({})).to_line();
            let back: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(back["id"], id);
        }
    }

    #[test]
    fn lines_split_on_newline_and_survive_crlf() {
        let mut r = std::io::BufReader::new(&b"one\r\ntwo\n"[..]);
        let mut buf = Vec::new();
        for expected in ["one", "two"] {
            match read_line(&mut r, &mut buf).unwrap() {
                Line::Message(m) => assert_eq!(String::from_utf8(m).unwrap(), expected),
                _ => panic!("expected a message"),
            }
        }
        assert!(matches!(read_line(&mut r, &mut buf).unwrap(), Line::Eof));
    }

    #[test]
    fn an_unbounded_line_is_refused_before_it_is_all_in_memory() {
        // Twice the cap, no newline anywhere.
        let flood = vec![b'x'; MAX_LINE_BYTES * 2];
        let mut r = std::io::BufReader::with_capacity(8 * 1024, &flood[..]);
        let mut buf = Vec::new();
        assert!(matches!(
            read_line(&mut r, &mut buf).unwrap(),
            Line::TooLong
        ));
        // The refusal has to happen near the cap, not after swallowing it all.
        assert!(
            buf.len() < MAX_LINE_BYTES + 64 * 1024,
            "buffered {} bytes before giving up",
            buf.len()
        );
    }
}
