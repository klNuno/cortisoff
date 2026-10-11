//! What the window draws, apart from the window itself.
//!
//! [`theme`] and [`widgets`] are the look every page shares. [`page`] runs a
//! plugin's window half: its `status` and `page` functions, in the same Lua
//! sandbox the daemon half runs in, drawing with those widgets. The app's
//! shell owns the sidebar, the plugin header and the connection to the
//! daemon; everything inside a plugin page comes from here.

pub mod page;
pub mod theme;
pub mod widgets;

pub use page::{Effect, Page, State, Status};
