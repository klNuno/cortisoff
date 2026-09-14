//! The wire between the daemon and everything that talks to it.
//!
//! The CLI, the Tauri dashboard and any third-party client are all IPC
//! clients and share no code with the daemon beyond this crate. That is what
//! keeps the daemon free of a webview and free of a windowing crate, and it
//! is why the dashboard can exist only while it is open.
//!
//! This crate is the protocol and the framing, both pure. The transport is
//! the platform's job, and it is never a localhost TCP socket: any process on
//! the machine can connect to one of those, and this connection can change
//! the user's display. Windows uses a named pipe with an SDDL granting the
//! session's own user and nobody else; everything else uses a unix socket at
//! 0600 under the user's runtime directory.
//!
//! It depends on `cortisoff-core` for one thing: a [`Rule`] travels over the
//! wire as itself, so an editing client and the daemon cannot end up with two
//! definitions of what a rule is. A client still links no actuator, which is
//! the property that keeps it from touching a screen.

#![forbid(unsafe_code)]
// `Condition` nests inside itself, so serde's generated code for a `Request`
// carrying rules goes deeper than the default limit allows.
#![recursion_limit = "1024"]

mod codec;
mod protocol;
mod value;

pub use codec::{decode, encode, Frame, FrameReader, ProtocolError, RequestId, MAX_FRAME_BYTES};
/// Re-exported so a client depends on this crate alone to read and write rules.
pub use cortisoff_core::{Assign, ClockTime, Condition, Rule};
pub use protocol::{
    CapabilityReport, ClaimReport, DoctorReport, Notification, Request, Response, RuleReport,
    RuleStateReport, StateReport, WorldReport,
};
pub use value::parse_value;
