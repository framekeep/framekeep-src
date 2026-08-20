//! Methods, and who is allowed to call them. This is S3.6.
//!
//! # What this boundary is for
//!
//! It stops the *model* from doing two things: putting new video into the
//! system (`video.ingest`) and approving its own redaction (`redaction.apply`).
//! Both belong to a human at the GUI, and the whole product falls over if a
//! chat transcript can talk its way into either.
//!
//! # What it is not for
//!
//! It is not a defence against other software running as this user. The pipe's
//! DACL admits that user's own processes, and any of them could claim to be the
//! tray in `hello` -- or skip the tray entirely and run `framekeep-core` itself.
//! Saying otherwise would be the kind of over-claim this project keeps catching
//! in its own documents.
//!
//! # Why it is shaped like this
//!
//! [`Method::allows`] is an exhaustive `match` over every method. Adding a
//! variant without saying who may call it does not compile. The rule cannot be
//! forgotten in a hurry, because the compiler asks before the reviewer does.
//!
//! Note the naming split, easy to trip on: MCP *tools* are `video_map` with an
//! underscore, IPC *methods* are `video.map` with a dot. Different surfaces.

/// Who is on the other end of the connection, taken from `client` in `hello`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// `framekeep-mcp` -- the adapter the chat model reaches through.
    Mcp,
    /// `framekeep-tray` -- the GUI, i.e. a human clicking.
    Tray,
    /// Anything else. Reads nothing, writes nothing.
    Unknown,
}

impl Caller {
    pub fn from_client_name(name: &str) -> Caller {
        match name {
            "framekeep-mcp" => Caller::Mcp,
            "framekeep-tray" => Caller::Tray,
            _ => Caller::Unknown,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Hello,
    QueueList,
    QueueGet,
    VideoMap,
    VideoFrames,
    VideoStatus,
    VideoIngest,
    RedactionApply,
}

impl Method {
    pub fn parse(name: &str) -> Option<Method> {
        Some(match name {
            "hello" => Method::Hello,
            "queue.list" => Method::QueueList,
            "queue.get" => Method::QueueGet,
            "video.map" => Method::VideoMap,
            "video.frames" => Method::VideoFrames,
            "video.status" => Method::VideoStatus,
            "video.ingest" => Method::VideoIngest,
            "redaction.apply" => Method::RedactionApply,
            _ => return None,
        })
    }

    pub fn wire_name(self) -> &'static str {
        match self {
            Method::Hello => "hello",
            Method::QueueList => "queue.list",
            Method::QueueGet => "queue.get",
            Method::VideoMap => "video.map",
            Method::VideoFrames => "video.frames",
            Method::VideoStatus => "video.status",
            Method::VideoIngest => "video.ingest",
            Method::RedactionApply => "redaction.apply",
        }
    }

    /// The whole rule, in one exhaustive match. Deny is the default: an
    /// unrecognised client gets nothing but the handshake.
    pub fn allows(self, caller: Caller) -> bool {
        use Caller::*;
        use Method::*;
        match self {
            // Every connection has to be able to introduce itself, including
            // one whose name this build has never heard of -- that is how it
            // finds out the protocol does not match.
            Hello => true,

            // Reading: the model may look at what is already in the system.
            QueueList | QueueGet | VideoStatus | VideoMap | VideoFrames => {
                matches!(caller, Mcp | Tray)
            }

            // Writing: a human at the GUI, and nobody else.
            //
            // Do not add `Mcp` here. If a future feature seems to need it, the
            // feature is wrong -- read AGENTS.md, "Ranh gioi quyen IPC".
            VideoIngest | RedactionApply => matches!(caller, Tray),
        }
    }

    /// Message for a refusal. It tells the model to stop trying and hand the
    /// job back to the person, because a bare `FORBIDDEN` makes models retry
    /// with variations of the same call.
    pub fn refusal_message(self) -> String {
        match self {
            Method::VideoIngest => "Only you can add a recording to Framekeep. \
                 Paste or drop it in the app, then ask me again."
                .to_string(),
            Method::RedactionApply => "Only you can approve a redaction. \
                 Open Framekeep, review what it found, and send it to chat from there."
                .to_string(),
            other => format!(
                "This client is not allowed to call {}. Nothing to retry -- this is by design.",
                other.wire_name()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two rules AGENTS.md calls a security bug if broken. Written as a
    /// table so a new method cannot quietly slip past by being added to a
    /// different list.
    #[test]
    fn the_model_can_never_ingest_video_or_approve_redaction() {
        for m in [Method::VideoIngest, Method::RedactionApply] {
            assert!(
                !m.allows(Caller::Mcp),
                "{} is reachable by mcp",
                m.wire_name()
            );
            assert!(
                !m.allows(Caller::Unknown),
                "{} is reachable by an unknown client",
                m.wire_name()
            );
            assert!(
                m.allows(Caller::Tray),
                "{} must stay reachable by the GUI",
                m.wire_name()
            );
        }
    }

    #[test]
    fn an_unknown_client_gets_the_handshake_and_nothing_else() {
        let every = [
            Method::Hello,
            Method::QueueList,
            Method::QueueGet,
            Method::VideoMap,
            Method::VideoFrames,
            Method::VideoStatus,
            Method::VideoIngest,
            Method::RedactionApply,
        ];
        for m in every {
            let allowed = m.allows(Caller::Unknown);
            assert_eq!(
                allowed,
                m == Method::Hello,
                "{} allowed={allowed} for an unknown client",
                m.wire_name()
            );
        }
    }

    #[test]
    fn the_mcp_adapter_can_read_everything_it_needs_for_the_two_step_mechanism() {
        for m in [
            Method::QueueList,
            Method::QueueGet,
            Method::VideoMap,
            Method::VideoFrames,
            Method::VideoStatus,
        ] {
            assert!(
                m.allows(Caller::Mcp),
                "{} must be readable by mcp",
                m.wire_name()
            );
        }
    }

    #[test]
    fn names_survive_a_round_trip() {
        for m in [
            Method::Hello,
            Method::QueueList,
            Method::QueueGet,
            Method::VideoMap,
            Method::VideoFrames,
            Method::VideoStatus,
            Method::VideoIngest,
            Method::RedactionApply,
        ] {
            assert_eq!(Method::parse(m.wire_name()), Some(m));
        }
        // The MCP tool names use underscores. Sending one here is a mistake,
        // and it has to read as one rather than silently matching.
        assert_eq!(Method::parse("video_map"), None);
        assert_eq!(Method::parse(""), None);
    }
}
