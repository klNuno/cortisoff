//! A plugin, as the window sees it.
//!
//! Every plugin gets the same three things from the shell: a line in the
//! sidebar, a row on the overview, and a page of its own that it draws
//! itself. None of them is special to the shell, and a new one is a new
//! implementation of [`PluginUi`], never an edit here.

mod automations;
mod profiles;
mod wallpaper;

use eframe::egui::{Color32, Ui};

/// What a plugin says about itself, for the sidebar and the overview.
pub struct Status {
    /// A few words for the sidebar: `3200 K until 07:00`.
    pub line: String,
    /// A sentence for the overview.
    pub detail: String,
    pub color: Color32,
}

pub trait PluginUi {
    fn name(&self) -> &str;
    fn icon(&self) -> char;
    /// Called once a frame before anything is drawn, for a plugin that
    /// watches files or state.
    fn poll(&mut self) {}
    fn status(&self) -> Status;
    /// The whole page, header included.
    fn page(&mut self, ui: &mut Ui);
}

/// The plugins the prototype shows, with made-up state.
pub fn mock() -> Vec<Box<dyn PluginUi>> {
    vec![
        // Night light is a Lua plugin, loaded from the repo's `plugins/`
        // folder the way an installed one would be from the data folder.
        Box::new(crate::script::LuaPlugin::load(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/night-light"),
            20 * 60 + 41,
        )),
        Box::new(profiles::DisplayProfiles::default()),
        Box::new(wallpaper::WallpaperPause::default()),
        Box::new(automations::Automations::default()),
    ]
}
