//! The wire between the daemon and everything that talks to it.
//!
//! The CLI, the window and any third-party client are all IPC
//! clients and share no code with the daemon beyond this crate. That is what
//! keeps the daemon's loop free of anything the window draws, and it is why
//! the window can exist only while it is open.
//!
//! This crate is the protocol and the framing, both pure. The transport is
//! the platform's job, and it is never a localhost TCP socket: any process on
//! the machine can connect to one of those, and this connection can change
//! the user's display. Windows uses a named pipe with an SDDL granting the
//! session's own user and nobody else; everything else uses a unix socket at
//! 0600 under the user's runtime directory.
//!
#![forbid(unsafe_code)]

mod codec;
mod protocol;
mod value;

pub use codec::{decode, encode, Frame, FrameReader, ProtocolError, RequestId, MAX_FRAME_BYTES};
pub use protocol::{
    CapabilityReport, ClaimReport, DoctorReport, Json, Notification, Origin, PluginReport, Request,
    Response, StateReport, WorldReport,
};
pub use value::parse_value;
