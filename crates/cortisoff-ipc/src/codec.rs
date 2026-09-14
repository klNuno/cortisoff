//! Framing: one JSON object per line.
//!
//! Chosen over a length prefix because it is debuggable by hand. A developer
//! can pipe a line into the socket and read the answer, and `cortisoff trace`
//! output stays greppable. `serde_json` escapes newlines inside strings, so a
//! frame can never contain one and the framing cannot be confused by content.

use serde::{Deserialize, Serialize};

use crate::protocol::{Notification, Request, Response};

/// Correlates a response with the request that caused it, because
/// notifications interleave with responses on the same connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RequestId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum Frame {
    Request { id: RequestId, request: Request },
    Response { id: RequestId, response: Response },
    Notification { notification: Notification },
}

impl Frame {
    pub fn request(id: u64, request: Request) -> Self {
        Frame::Request {
            id: RequestId(id),
            request,
        }
    }

    pub fn response(id: RequestId, response: Response) -> Self {
        Frame::Response { id, response }
    }

    pub fn notify(notification: Notification) -> Self {
        Frame::Notification { notification }
    }
}

/// A frame this size is either a bug or a client trying to exhaust the
/// daemon's memory. Comfortably above a doctor report on a machine with
/// hundreds of rules.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolError {
    /// The line was not a frame this version understands.
    Malformed(String),
    /// No newline arrived before the cap. The connection is dropped rather
    /// than trusted.
    FrameTooLarge { bytes: usize },
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Malformed(m) => write!(f, "malformed frame: {m}"),
            ProtocolError::FrameTooLarge { bytes } => write!(
                f,
                "frame exceeded {MAX_FRAME_BYTES} bytes without a newline ({bytes} buffered)"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// One frame, newline terminated, ready to write.
pub fn encode(frame: &Frame) -> String {
    let mut line = serde_json::to_string(frame).expect("a Frame is always serializable");
    line.push('\n');
    line
}

pub fn decode(line: &str) -> Result<Frame, ProtocolError> {
    serde_json::from_str(line).map_err(|e| ProtocolError::Malformed(e.to_string()))
}

/// Turns the arbitrary chunks a socket hands back into whole frames.
///
/// A read on a pipe splits wherever it likes, including in the middle of a
/// multi-byte character, so nothing may assume one read is one message.
#[derive(Debug, Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed what a read returned. Frames come back in arrival order; a
    /// trailing partial frame stays buffered for the next call.
    ///
    /// A malformed line yields an error for that line only and the reader
    /// stays usable, so one bad frame does not kill a subscription that has
    /// been open for hours.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<Frame, ProtocolError>> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();

        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = &line[..end];
            // A keepalive or a stray blank line is not an error.
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            out.push(match std::str::from_utf8(line) {
                Ok(s) => decode(s),
                Err(e) => Err(ProtocolError::Malformed(e.to_string())),
            });
        }

        if self.buffer.len() > MAX_FRAME_BYTES {
            let bytes = self.buffer.len();
            self.buffer.clear();
            out.push(Err(ProtocolError::FrameTooLarge { bytes }));
        }
        out
    }

    /// Bytes held back waiting for their newline.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortisoff_host::{ids, CapabilityId, LogLevel, Value};

    fn set_frame() -> Frame {
        Frame::request(
            7,
            Request::Set {
                cap: CapabilityId::new(ids::DISPLAY_VIBRANCE),
                value: Value::Int(100),
            },
        )
    }

    fn frames(reader: &mut FrameReader, chunk: &[u8]) -> Vec<Frame> {
        reader
            .push(chunk)
            .into_iter()
            .map(|r| r.expect("should decode"))
            .collect()
    }

    #[test]
    fn a_frame_survives_a_round_trip() {
        let frame = set_frame();
        assert_eq!(decode(encode(&frame).trim_end()).unwrap(), frame);
    }

    /// Every variant, not just the ones a test happened to build.
    ///
    /// `encode` panics rather than returning an error, on the argument that a
    /// frame is always serializable. That argument is only true if nothing in
    /// the protocol is a shape serde refuses: a newtype variant holding a
    /// sequence inside an internally tagged enum compiles fine and then kills
    /// the daemon the first time a client asks for it. That is exactly what
    /// `Response::Rules` was before this test existed.
    #[test]
    fn every_request_and_response_shape_encodes_and_comes_back() {
        let rule = crate::Rule {
            name: "a rule with every kind of condition".to_owned(),
            priority: 100,
            enabled: false,
            when: crate::Condition::All(vec![
                crate::Condition::Between {
                    start: "20:00".parse().expect("a time of day"),
                    end: "07:00".parse().expect("a time of day"),
                },
                crate::Condition::Not(Box::new(crate::Condition::Any(vec![
                    crate::Condition::App {
                        exe: Some("vlc.exe".to_owned()),
                        title_contains: None,
                    },
                    crate::Condition::Fullscreen(true),
                ]))),
                crate::Condition::Idle(false),
            ]),
            set: vec![crate::Assign {
                cap: CapabilityId::new("display.color_temp"),
                value: Value::Int(3200),
            }],
            transition: Some(cortisoff_core::Transition {
                seconds: 40 * 60,
                offset: 0,
                from: vec![crate::Assign {
                    cap: CapabilityId::new("display.color_temp"),
                    value: Value::Int(6600),
                }],
            }),
        };

        let requests = [
            Request::Doctor,
            Request::State,
            Request::Subscribe,
            Request::Unsubscribe,
            Request::Rules,
            Request::SaveRules {
                rules: vec![rule.clone()],
            },
            Request::Set {
                cap: CapabilityId::new("display.mode"),
                value: Value::Mode {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 240_000,
                },
            },
            Request::Clear {
                cap: CapabilityId::new("display.vibrance"),
            },
            Request::Preview {
                cap: CapabilityId::new("display.color_temp"),
                value: Value::Int(2700),
            },
            Request::PreviewEnd {
                cap: CapabilityId::new("display.color_temp"),
            },
            Request::Reload,
            Request::Shutdown,
        ];
        for request in requests {
            let frame = Frame::request(1, request);
            assert_eq!(decode(encode(&frame).trim_end()).unwrap(), frame);
        }

        let responses = [
            Response::Ok,
            Response::error("no"),
            Response::Doctor(Box::new(crate::DoctorReport {
                capabilities: vec![
                    crate::CapabilityReport {
                        id: CapabilityId::new("display.color_temp"),
                        backend: Some("gamma".to_owned()),
                        missing_because: None,
                        settable: true,
                        label: "Colour temperature".to_owned(),
                        control: Some(cortisoff_host::Control::Range {
                            min: 1500,
                            max: 6600,
                            step: 50,
                            default: 3400,
                            unit: Some("K".to_owned()),
                        }),
                    },
                    crate::CapabilityReport {
                        id: CapabilityId::new("wallpaper.playback"),
                        backend: None,
                        missing_because: Some("no engine".to_owned()),
                        settable: false,
                        label: "Wallpaper playback".to_owned(),
                        control: None,
                    },
                ],
                ..Default::default()
            })),
            Response::State(Box::default()),
            Response::Rules { rules: vec![rule] },
        ];
        for response in responses {
            let frame = Frame::response(RequestId(1), response);
            assert_eq!(decode(encode(&frame).trim_end()).unwrap(), frame);
        }
    }

    #[test]
    fn every_encoded_frame_is_exactly_one_line() {
        let line = encode(&Frame::notify(Notification::Log {
            level: LogLevel::Warn,
            // A newline inside a string must not break the framing.
            message: "two\nlines".into(),
        }));
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
    }

    /// A pipe splits where it likes. Nothing may assume one read is one
    /// message.
    #[test]
    fn a_frame_split_across_reads_is_reassembled() {
        let encoded = encode(&set_frame());
        let (head, tail) = encoded.as_bytes().split_at(9);

        let mut reader = FrameReader::new();
        assert!(frames(&mut reader, head).is_empty());
        assert_ne!(reader.pending(), 0);
        assert_eq!(frames(&mut reader, tail), vec![set_frame()]);
        assert_eq!(reader.pending(), 0);
    }

    #[test]
    fn several_frames_in_one_read_come_back_in_order() {
        let mut buf = encode(&Frame::request(1, Request::Doctor));
        buf.push_str(&encode(&Frame::request(2, Request::State)));
        buf.push_str(&encode(&Frame::request(3, Request::Subscribe)));

        let mut reader = FrameReader::new();
        assert_eq!(
            frames(&mut reader, buf.as_bytes()),
            vec![
                Frame::request(1, Request::Doctor),
                Frame::request(2, Request::State),
                Frame::request(3, Request::Subscribe),
            ]
        );
    }

    /// One bad frame must not end a subscription.
    #[test]
    fn a_malformed_line_is_reported_and_the_reader_keeps_working() {
        let mut buf = String::from("{ not json\n");
        buf.push_str(&encode(&Frame::request(1, Request::Doctor)));

        let mut reader = FrameReader::new();
        let got = reader.push(buf.as_bytes());
        assert!(matches!(got[0], Err(ProtocolError::Malformed(_))));
        assert_eq!(
            got[1].as_ref().unwrap(),
            &Frame::request(1, Request::Doctor)
        );
    }

    #[test]
    fn blank_lines_are_ignored_rather_than_reported() {
        let mut reader = FrameReader::new();
        assert!(reader.push(b"\n   \n\n").is_empty());
    }

    /// A client that never sends a newline must not grow the daemon's memory
    /// without bound.
    #[test]
    fn a_frame_with_no_newline_is_capped_and_the_buffer_dropped() {
        let mut reader = FrameReader::new();
        let flood = vec![b'x'; MAX_FRAME_BYTES + 1];
        let got = reader.push(&flood);
        assert!(matches!(
            got.as_slice(),
            [Err(ProtocolError::FrameTooLarge { .. })]
        ));
        assert_eq!(reader.pending(), 0);
    }

    /// Responses and notifications share the connection, so a client matches
    /// answers by id rather than by arrival order.
    #[test]
    fn a_response_carries_the_id_of_its_request() {
        let Frame::Request { id, .. } = set_frame() else {
            unreachable!()
        };
        let decoded = decode(encode(&Frame::response(id, Response::Ok)).trim_end()).unwrap();
        assert_eq!(decoded, Frame::response(RequestId(7), Response::Ok));
    }

    #[test]
    fn a_notification_carries_no_id() {
        let line = encode(&Frame::notify(Notification::Closing));
        assert!(!line.contains("\"id\""), "{line}");
    }
}
