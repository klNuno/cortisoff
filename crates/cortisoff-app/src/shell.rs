//! The window around the plugins: the sidebar, the overview, the page that
//! adds a plugin, cortisoff's own settings, and the link to the daemon.
//!
//! Every plugin is listed and opened the same way. The shell draws a
//! plugin's header and its on switch; everything under that is the plugin's
//! own page, run by [`cortisoff_ui::Page`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use cortisoff_host::{CapabilityId, LogLevel, Value};
use cortisoff_ipc::{
    ClaimReport, DoctorReport, Notification, Origin, PluginReport, Request, Response, StateReport,
    WorldReport,
};
use cortisoff_ui::theme::{self, glyph};
use cortisoff_ui::{widgets, Effect, Page, State, Status};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Frame, Layout, Margin, RichText, ScrollArea, Ui,
};

use crate::client::{Ask, Inbound, Link};

/// What the main area shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum View {
    Overview,
    /// A plugin, by id.
    Plugin(String),
    Add,
    Settings,
}

/// A settings edit is sent once the pointer lets go and the keyboard rests
/// this long, so a drag or a word typed is one write, not forty.
const SETTLE: Duration = Duration::from_millis(300);
/// How long a notice stays up.
const NOTICE_FOR: Duration = Duration::from_secs(6);
/// Log lines kept for the overview.
const RECENT: usize = 40;
/// A start or a stop asked from here shows as done this long before the
/// daemon confirms it; past that, the window believes the daemon again.
const IN_TRANSIT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Daemon {
    /// Nothing heard yet: the first connection attempt is under way.
    Unknown,
    Up,
    Down,
}

struct Entry {
    report: PluginReport,
    page: Page,
}

struct Pending {
    id: String,
    settings: serde_json::Value,
    since: Instant,
}

struct Notice {
    text: String,
    error: bool,
    at: Instant,
}

struct LogLine {
    time: String,
    level: LogLevel,
    text: String,
}

/// The release check, from the settings page.
enum Update {
    Idle,
    Checking,
    Current,
    Available(cortisoff_update::Release),
    Downloading(String),
    Ready { staged: PathBuf, version: String },
    Restarting,
    Failed(String),
}

/// What a background thread hands back.
enum Done {
    Installed(Result<(String, String), String>),
    Checked(Result<Option<cortisoff_update::Release>, String>),
    Downloaded(Result<(PathBuf, String), String>),
    /// The new version is in place and started; this window closes.
    Restarted(Result<(), String>),
    Uninstalled(Result<(), String>),
}

pub struct Shell {
    pub view: View,
    link: Option<Link>,
    daemon: Daemon,
    entries: Vec<Entry>,
    state: Option<StateReport>,
    doctor: Option<DoctorReport>,
    recent: VecDeque<LogLine>,
    notice: Option<Notice>,
    pending: Vec<Pending>,
    /// A world change arrived and the overview's copy is stale.
    state_stale: bool,
    state_asked: bool,
    /// When the daemon was started from here, so the page can say so.
    starting: Option<Instant>,
    /// When the daemon was asked to stop from here, so the logo turns at once.
    stopping: Option<Instant>,
    /// Open this plugin as soon as the list carries it: one just added.
    open_when_listed: Option<String>,
    /// A linked folder was sent; open whichever plugin is new in the next
    /// list.
    open_new: Option<Vec<String>>,
    add_text: String,
    installing: bool,
    at_logon: Option<bool>,
    update: Update,
    confirm: Option<&'static str>,
    jobs: (Sender<Done>, Receiver<Done>),
    themed: bool,
    /// Fixed clock and log for a capture; the real ones otherwise.
    demo: bool,
}

impl Shell {
    /// The window, talking to the session's daemon.
    pub fn live(ctx: &egui::Context) -> Shell {
        let wake = ctx.clone();
        let link = Link::start(move || wake.request_repaint());
        Shell::with(Some(link), false)
    }

    fn with(link: Option<Link>, demo: bool) -> Shell {
        Shell {
            view: View::Overview,
            link,
            daemon: Daemon::Unknown,
            entries: Vec::new(),
            state: None,
            doctor: None,
            recent: VecDeque::new(),
            notice: None,
            pending: Vec::new(),
            state_stale: false,
            state_asked: false,
            starting: None,
            stopping: None,
            open_when_listed: None,
            open_new: None,
            add_text: String::new(),
            installing: false,
            at_logon: None,
            update: Update::Idle,
            confirm: None,
            jobs: channel(),
            themed: false,
            demo,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        // Fonts set now bind at the start of the next frame, and a named
        // family used before that panics. The first frame draws nothing.
        if !self.themed {
            theme::install(ui.ctx());
            self.themed = true;
            ui.ctx().request_repaint();
            return;
        }
        self.receive();
        self.finish_jobs(ui.ctx());
        self.flush_settings(ui);
        if self.state_stale && !self.state_asked {
            self.ask(Ask::State, Request::State);
        }
        let now = self.now();

        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(248.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::SIDEBAR)
                    .inner_margin(Margin::symmetric(10, 14)),
            )
            .show(ui, |ui| self.sidebar(ui, now));
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .auto_shrink(false)
                    .id_salt(self.view.clone())
                    .show(ui, |ui| {
                        Frame::new()
                            .inner_margin(Margin {
                                left: 32,
                                right: 32,
                                top: 26,
                                bottom: 32,
                            })
                            .show(ui, |ui| {
                                ui.set_max_width(ui.available_width().min(760.0));
                                self.daemon_banner(ui);
                                match self.view.clone() {
                                    View::Overview => self.overview(ui, now),
                                    View::Plugin(id) => self.plugin(ui, &id, now),
                                    View::Add => self.add(ui),
                                    View::Settings => self.settings(ui),
                                }
                            });
                    });
            });
        self.notice(ui.ctx());

        // Status lines read the clock to the minute, so the page is drawn again
        // when the minute turns and not before. A pending edit has a deadline,
        // and a start or a stop in transit falls back to the daemon's own
        // state once it is no longer recent.
        if matches!(self.view, View::Plugin(_)) || self.view == View::Overview {
            ui.ctx().request_repaint_after(until_next_minute());
        }
        if !self.pending.is_empty() {
            ui.ctx().request_repaint_after(SETTLE);
        }
        for asked in [self.starting, self.stopping].into_iter().flatten() {
            if let Some(left) = IN_TRANSIT.checked_sub(asked.elapsed()) {
                ui.ctx().request_repaint_after(left);
            }
        }
    }

    // ---- the daemon ----------------------------------------------------

    fn ask(&mut self, ask: Ask, request: Request) {
        if ask == Ask::State {
            self.state_asked = true;
            self.state_stale = false;
        }
        if let Some(link) = &self.link {
            link.ask(ask, request);
        }
    }

    fn receive(&mut self) {
        let Some(link) = &self.link else { return };
        for inbound in link.drain() {
            match inbound {
                Inbound::Connected => {
                    self.daemon = Daemon::Up;
                    self.starting = None;
                    self.stopping = None;
                    self.ask(Ask::Plugins, Request::Plugins);
                    self.ask(Ask::State, Request::State);
                    self.ask(Ask::Doctor, Request::Doctor);
                }
                Inbound::Disconnected => {
                    self.daemon = Daemon::Down;
                    self.stopping = None;
                    self.state = None;
                    self.state_asked = false;
                }
                Inbound::Notification(n) => self.notified(n),
                Inbound::Answer(ask, response) => self.answered(ask, response),
                Inbound::Failed(ask, error) => {
                    if ask == Ask::State {
                        self.state_asked = false;
                    }
                    if let Ask::Do(what) = ask {
                        self.say(format!("Could not {what}: {error}"), true);
                    }
                }
            }
        }
    }

    fn notified(&mut self, notification: Notification) {
        match notification {
            Notification::Plugin(report) => self.adopt(*report),
            Notification::Plugins => self.ask(Ask::Plugins, Request::Plugins),
            Notification::Event(_) | Notification::Action(_) => self.state_stale = true,
            Notification::Log { level, message } => self.log(level, message),
            Notification::Closing => {
                self.daemon = Daemon::Down;
                self.state = None;
            }
        }
    }

    fn answered(&mut self, ask: Ask, response: Response) {
        match (ask, response) {
            (Ask::State, Response::State(state)) => {
                self.state_asked = false;
                for report in &state.plugins {
                    self.adopt(report.clone());
                }
                self.state = Some(*state);
            }
            (Ask::Plugins, Response::Plugins { plugins }) => self.list(plugins),
            (Ask::Doctor, Response::Doctor(doctor)) => self.doctor = Some(*doctor),
            (ask, Response::Error { message }) => {
                if ask == Ask::State {
                    self.state_asked = false;
                }
                let what = match ask {
                    Ask::Do(what) => what,
                    _ => "ask the daemon".to_owned(),
                };
                self.say(format!("Could not {what}: {message}"), true);
            }
            _ => {}
        }
    }

    /// One plugin's fresh report.
    fn adopt(&mut self, report: PluginReport) {
        match self.entries.iter_mut().find(|e| e.report.id == report.id) {
            Some(entry) => {
                entry.page.sync(&report);
                entry.report = report;
            }
            None => self.entries.push(Entry {
                page: Page::new(&report),
                report,
            }),
        }
    }

    /// The whole list, in the daemon's order.
    fn list(&mut self, plugins: Vec<PluginReport>) {
        let known: Vec<String> = self.entries.iter().map(|e| e.report.id.clone()).collect();
        let mut old: Vec<Entry> = std::mem::take(&mut self.entries);
        for report in plugins {
            match old.iter().position(|e| e.report.id == report.id) {
                Some(i) => {
                    let mut entry = old.swap_remove(i);
                    entry.page.sync(&report);
                    entry.report = report;
                    self.entries.push(entry);
                }
                None => self.entries.push(Entry {
                    page: Page::new(&report),
                    report,
                }),
            }
        }
        if let View::Plugin(id) = &self.view {
            if !self.entries.iter().any(|e| &e.report.id == id) {
                self.view = View::Overview;
            }
        }
        if let Some(id) = &self.open_when_listed {
            if self.entries.iter().any(|e| &e.report.id == id) {
                self.view = View::Plugin(id.clone());
                self.open_when_listed = None;
            }
        }
        if let Some(before) = self.open_new.take() {
            match self.entries.iter().find(|e| !before.contains(&e.report.id)) {
                Some(new) => self.view = View::Plugin(new.report.id.clone()),
                None if before == known => self.open_new = Some(before),
                None => {}
            }
        }
    }

    fn log(&mut self, level: LogLevel, text: String) {
        if level == LogLevel::Trace {
            return;
        }
        let (_, _, _, h, m, _, _) = local_time();
        self.recent.push_front(LogLine {
            time: format!("{h:02}:{m:02}"),
            level,
            text,
        });
        self.recent.truncate(RECENT);
    }

    /// A problem from before the window opened, shown once it has.
    pub fn warn(&mut self, text: String) {
        self.say(text, true);
    }

    fn say(&mut self, text: String, error: bool) {
        self.notice = Some(Notice {
            text,
            error,
            at: Instant::now(),
        });
    }

    /// Settings a page edited go out once the edit rests.
    fn flush_settings(&mut self, ui: &Ui) {
        if self.pending.is_empty() {
            return;
        }
        let (pressed, typing) = ui.input(|i| (i.pointer.any_down(), !i.events.is_empty()));
        let busy = pressed || (typing && ui.ctx().egui_wants_keyboard_input());
        let due: Vec<Pending> = {
            let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
                .into_iter()
                .partition(|p| !busy || p.since.elapsed() >= SETTLE);
            self.pending = wait;
            due
        };
        for p in due {
            self.ask(
                Ask::Do("save the settings".to_owned()),
                Request::Settings {
                    id: p.id,
                    settings: p.settings,
                },
            );
        }
    }

    fn start_daemon(&mut self) {
        let started = std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| cortisoff_daemon::spawn_detached(&exe));
        match started {
            Ok(()) => self.starting = Some(Instant::now()),
            Err(e) => self.say(format!("Could not start cortisoff: {e}"), true),
        }
    }

    /// Stops the daemon, which releases every claim on the way out.
    fn stop_daemon(&mut self) {
        self.ask(Ask::Do("stop cortisoff".to_owned()), Request::Shutdown);
        self.stopping = Some(Instant::now());
    }

    /// Whether cortisoff is on as the logo shows it: running, or asked to
    /// start from here. `None` until the daemon has answered or failed to.
    fn switched_on(&self) -> Option<bool> {
        if recent(self.stopping) {
            return Some(false);
        }
        if recent(self.starting) {
            return Some(true);
        }
        match self.daemon {
            Daemon::Unknown => None,
            Daemon::Up => Some(true),
            Daemon::Down => Some(false),
        }
    }

    /// The logo's click: stop what runs, start what does not.
    fn toggle(&mut self) {
        if recent(self.starting) || recent(self.stopping) {
            return;
        }
        match self.switched_on() {
            Some(true) => self.stop_daemon(),
            Some(false) => self.start_daemon(),
            None => {}
        }
    }

    // ---- background work -----------------------------------------------

    fn spawn(&self, ctx: &egui::Context, work: impl FnOnce() -> Done + Send + 'static) {
        let tx = self.jobs.0.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    fn finish_jobs(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.jobs.1.try_recv() {
            match done {
                Done::Installed(result) => {
                    self.installing = false;
                    match result {
                        Ok((id, name)) => {
                            self.add_text.clear();
                            self.open_when_listed = Some(id);
                            self.ask(Ask::Do("load it".to_owned()), Request::Rescan);
                            self.say(
                                format!("{name} is installed. Switch it on when ready."),
                                false,
                            );
                        }
                        Err(e) => self.say(e, true),
                    }
                }
                Done::Checked(Ok(None)) => self.update = Update::Current,
                Done::Checked(Ok(Some(release))) => self.update = Update::Available(release),
                Done::Checked(Err(e)) | Done::Downloaded(Err(e)) => self.update = Update::Failed(e),
                Done::Downloaded(Ok((staged, version))) => {
                    self.update = Update::Ready { staged, version }
                }
                Done::Restarted(Ok(())) | Done::Uninstalled(Ok(())) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close)
                }
                Done::Restarted(Err(e)) => self.update = Update::Failed(e),
                Done::Uninstalled(Err(e)) => self.say(format!("Uninstall: {e}"), true),
            }
        }
    }

    fn install(&mut self, ctx: &egui::Context, link: String) {
        let Some(plugins) = plugins_dir() else {
            self.say("cannot find the plugin folder".to_owned(), true);
            return;
        };
        self.installing = true;
        self.spawn(ctx, move || {
            Done::Installed(
                cortisoff_runtime::install::install(&link, &plugins)
                    .map(|i| (i.manifest.id, i.manifest.name)),
            )
        });
    }

    // ---- drawing -------------------------------------------------------

    fn now(&self) -> f64 {
        if self.demo {
            return 21.0 * 60.0 + 12.0;
        }
        let (_, _, _, h, m, s, ms) = local_time();
        f64::from(h) * 60.0 + f64::from(m) + f64::from(s) / 60.0 + f64::from(ms) / 60_000.0
    }

    fn sidebar(&mut self, ui: &mut Ui, now: f64) {
        ui.spacing_mut().item_spacing.y = 2.0;
        let busy = recent(self.starting) || recent(self.stopping);
        if switch(ui, self.switched_on(), busy).clicked() {
            self.toggle();
        }
        ui.add_space(10.0);
        if widgets::nav_item(
            ui,
            glyph::HOME,
            "Overview",
            None,
            self.view == View::Overview,
        )
        .clicked()
        {
            self.view = View::Overview;
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("Plugins")
                    .font(theme::body(12.5))
                    .color(theme::SUBTLE),
            );
        });
        ui.add_space(4.0);

        let bottom = 2.0 * 36.0 + 8.0;
        let list = ui.available_height() - bottom;
        ScrollArea::vertical()
            .max_height(list.max(48.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if self.entries.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        widgets::hint(
                            ui,
                            match self.daemon {
                                Daemon::Down => "Start cortisoff to list them.",
                                _ => "None yet.",
                            },
                        );
                    });
                }
                for i in 0..self.entries.len() {
                    let (line, color) = self.line(i, now);
                    let entry = &self.entries[i];
                    let selected = self.view == View::Plugin(entry.report.id.clone());
                    let icon = glyph::named(entry.report.icon.as_deref().unwrap_or(""));
                    if widgets::nav_item(
                        ui,
                        icon,
                        &entry.report.name,
                        Some((&line, color)),
                        selected,
                    )
                    .clicked()
                    {
                        self.view = View::Plugin(entry.report.id.clone());
                    }
                }
            });

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            if widgets::nav_item(
                ui,
                glyph::SETTINGS,
                "Settings",
                None,
                self.view == View::Settings,
            )
            .clicked()
            {
                self.view = View::Settings;
                self.at_logon = Some(cortisoff_daemon::starts_at_logon());
                self.ask(Ask::Doctor, Request::Doctor);
            }
            if widgets::nav_item(ui, glyph::ADD, "Add a plugin", None, self.view == View::Add)
                .clicked()
            {
                self.view = View::Add;
            }
        });
    }

    /// A plugin's one-line status and its colour, the same in the sidebar
    /// and on the overview.
    fn line(&mut self, i: usize, now: f64) -> (String, Color32) {
        let r = &self.entries[i].report;
        if !r.supported {
            return ("Not for this system".to_owned(), theme::SUBTLE);
        }
        if !r.enabled {
            return ("Off".to_owned(), theme::SUBTLE);
        }
        // What the page would say comes from the last state published, which
        // no longer holds once the daemon is gone.
        if self.daemon == Daemon::Down {
            return ("Not running".to_owned(), theme::WAITING);
        }
        if r.error.is_some() && !r.running {
            return ("Failed".to_owned(), theme::FAILED);
        }
        match self.entries[i].page.status(now) {
            Some(status) => (status.line, color(status.state)),
            None => ("On".to_owned(), theme::HOLDING),
        }
    }

    fn status(&mut self, i: usize, now: f64) -> Status {
        let (line, colour) = self.line(i, now);
        let r = &self.entries[i].report;
        let enabled = r.enabled && r.supported && self.daemon != Daemon::Down;
        let status = if enabled {
            self.entries[i].page.status(now)
        } else {
            None
        };
        status.unwrap_or(Status {
            line,
            detail: String::new(),
            state: if colour == theme::FAILED {
                State::Failed
            } else {
                State::Waiting
            },
        })
    }

    /// Above every page while no daemon answers: the window shows the last
    /// thing it knew and says so.
    fn daemon_banner(&mut self, ui: &mut Ui) {
        if self.daemon != Daemon::Down {
            return;
        }
        let starting = recent(self.starting);
        let mut start = false;
        Frame::new()
            .fill(theme::CARD)
            .stroke(egui::Stroke::new(1.0, theme::CARD_BORDER))
            .corner_radius(theme::RADIUS_CARD)
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    widgets::dot(ui, theme::PAUSED);
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(if starting {
                            "Starting cortisoff"
                        } else {
                            "cortisoff is not running"
                        })
                        .font(theme::strong(14.0))
                        .color(theme::TEXT),
                    );
                    ui.label(
                        RichText::new("Plugins do nothing until it runs.")
                            .font(theme::body(13.0))
                            .color(theme::MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !starting && widgets::button(ui, Some(glyph::ARROW), "Start").clicked() {
                            start = true;
                        }
                    });
                });
            });
        ui.add_space(14.0);
        if start {
            self.start_daemon();
        }
    }

    fn overview(&mut self, ui: &mut Ui, now: f64) {
        let on = self
            .entries
            .iter()
            .filter(|e| e.report.enabled && e.report.supported)
            .count();
        let summary = match (self.daemon, on) {
            (Daemon::Down, _) => "Nothing runs right now.".to_owned(),
            (_, 0) => "No plugin is on.".to_owned(),
            (_, 1) => "1 plugin on.".to_owned(),
            (_, n) => format!("{n} plugins on."),
        };
        widgets::page_header(ui, glyph::HOME, "Overview", &summary, |_| {});

        widgets::card(ui, Some("Right now"), |ui| {
            let shown: Vec<usize> = (0..self.entries.len())
                .filter(|&i| self.entries[i].report.enabled && self.entries[i].report.supported)
                .collect();
            if shown.is_empty() {
                widgets::hint(
                    ui,
                    "Open a plugin on the left and switch it on. Add more with Add a plugin.",
                );
            }
            for (n, i) in shown.into_iter().enumerate() {
                if n > 0 {
                    widgets::divider(ui);
                }
                let status = self.status(i, now);
                let entry = &self.entries[i];
                let icon = glyph::named(entry.report.icon.as_deref().unwrap_or(""));
                let name = entry.report.name.clone();
                let id = entry.report.id.clone();
                ui.add_space(2.0);
                let row = ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 40.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(icon)
                                .font(theme::icon(16.0))
                                .color(theme::MUTED),
                        );
                        ui.add_space(6.0);
                        let title = RichText::new(&name)
                            .font(theme::strong(14.0))
                            .color(theme::TEXT);
                        // A vertical block takes the row's whole height, so a
                        // name alone is drawn straight in the row to centre.
                        if status.detail.is_empty() {
                            ui.label(title);
                        } else {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(title);
                                ui.label(
                                    RichText::new(&status.detail)
                                        .font(theme::body(12.5))
                                        .color(theme::MUTED),
                                );
                            });
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(&status.line)
                                    .font(theme::body(13.0))
                                    .color(theme::TEXT),
                            );
                            widgets::dot(ui, color(status.state));
                        });
                    },
                );
                if row
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.view = View::Plugin(id);
                }
                ui.add_space(2.0);
            }
        });
        ui.add_space(12.0);

        let claims: Vec<ClaimReport> = self
            .state
            .as_ref()
            .map(|s| s.claims.clone())
            .unwrap_or_default();
        if !claims.is_empty() {
            widgets::card(ui, Some("Holding"), |ui| {
                for claim in &claims {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(self.cap_label(&claim.cap))
                                .font(theme::body(13.5))
                                .color(theme::TEXT),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(self.owner_name(&claim.owner))
                                    .font(theme::body(12.5))
                                    .color(theme::MUTED),
                            );
                            ui.label(
                                RichText::new(show_value(&claim.cap, &claim.value))
                                    .font(theme::strong(13.5))
                                    .color(theme::TEXT),
                            );
                        });
                    });
                }
            });
            ui.add_space(12.0);
        }

        if let Some(world) = self.state.as_ref().map(|s| s.world.clone()) {
            widgets::card(ui, Some("What the machine says"), |ui| {
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| world_chips(ui, &world));
                ui.add_space(4.0);
                widgets::hint(ui, "Only what an enabled plugin listens to is watched.");
            });
            ui.add_space(12.0);
        }

        widgets::card(ui, Some("Recent"), |ui| {
            if self.recent.is_empty() {
                widgets::hint(ui, "Nothing yet.");
            }
            for line in self.recent.iter().take(12) {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&line.time)
                            .font(theme::body(13.0))
                            .color(theme::SUBTLE),
                    );
                    ui.add_space(8.0);
                    let colour = match line.level {
                        LogLevel::Error => theme::FAILED,
                        LogLevel::Warn => theme::PAUSED,
                        _ => theme::TEXT,
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(&line.text)
                                .font(theme::body(13.5))
                                .color(colour),
                        )
                        .truncate(),
                    );
                });
            }
        });
    }

    fn plugin(&mut self, ui: &mut Ui, id: &str, now: f64) {
        let Some(i) = self.entries.iter().position(|e| e.report.id == id) else {
            self.view = View::Overview;
            return;
        };
        let report = self.entries[i].report.clone();
        let icon = glyph::named(report.icon.as_deref().unwrap_or(""));
        let mut enabled = report.enabled;
        let mut toggled = false;
        widgets::page_header(ui, icon, &report.name, &report.description, |ui| {
            ui.add_enabled_ui(report.supported && self.daemon == Daemon::Up, |ui| {
                toggled = widgets::switch(ui, &mut enabled)
                    .on_hover_text(if enabled { "On" } else { "Off" })
                    .changed();
            });
        });
        if toggled {
            self.entries[i].report.enabled = enabled;
            self.ask(
                Ask::Do(format!(
                    "switch {} {}",
                    report.name,
                    if enabled { "on" } else { "off" }
                )),
                Request::Enable {
                    id: report.id.clone(),
                    enabled,
                },
            );
        }

        if !report.supported {
            note(
                ui,
                theme::SUBTLE,
                &format!("Made for {}.", list(&report.platforms)),
            );
        }
        if let Some(error) = report.error.as_deref().filter(|_| report.enabled) {
            note(ui, theme::FAILED, error);
        }
        if report.ffi {
            note(
                ui,
                theme::PAUSED,
                "Uses native calls: it can do anything your account can.",
            );
        }

        let effects = self.entries[i].page.draw(ui, now);
        self.apply(&report.id, effects);
        ui.add_space(12.0);
        self.about(ui, &report);
    }

    fn apply(&mut self, id: &str, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Settings(settings) => {
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.report.id == id) {
                        entry.report.settings = settings.clone();
                    }
                    self.pending.retain(|p| p.id != id);
                    self.pending.push(Pending {
                        id: id.to_owned(),
                        settings,
                        since: Instant::now(),
                    });
                }
                Effect::Send { name, data } => self.ask(
                    Ask::Do(format!("send {name}")),
                    Request::Message {
                        id: id.to_owned(),
                        name,
                        data,
                    },
                ),
                Effect::Preview { cap, value } => self.ask(
                    Ask::Do("preview".to_owned()),
                    Request::Preview { cap, value },
                ),
                Effect::PreviewEnd { cap } => self.ask(
                    Ask::Do("end the preview".to_owned()),
                    Request::PreviewEnd { cap },
                ),
            }
        }
    }

    /// Where a plugin comes from, and the way out.
    fn about(&mut self, ui: &mut Ui, report: &PluginReport) {
        let ctx = ui.ctx().clone();
        let mut remove = false;
        let mut update = None;
        widgets::card(ui, None, |ui| {
            ui.horizontal(|ui| {
                let mut line = format!("Version {}", report.version);
                if let Some(author) = &report.author {
                    line.push_str(&format!(" by {author}"));
                }
                ui.label(
                    RichText::new(line)
                        .font(theme::body(13.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(match report.origin {
                        Origin::Builtin => "Built in",
                        Origin::Installed => "Installed",
                        Origin::Linked => "Linked folder",
                    })
                    .font(theme::body(13.0))
                    .color(theme::SUBTLE),
                )
                .on_hover_text(
                    report
                        .url
                        .clone()
                        .or_else(|| report.dir.clone())
                        .unwrap_or_else(|| "Ships with cortisoff".to_owned()),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if report.origin == Origin::Builtin {
                        return;
                    }
                    let key = "remove";
                    let label = match (self.confirm == Some(key), report.origin) {
                        (true, _) => "Click again to confirm",
                        (false, Origin::Linked) => "Unlink",
                        (false, _) => "Remove",
                    };
                    if widgets::button(ui, Some(glyph::CLOSE), label).clicked() {
                        if self.confirm == Some(key) {
                            remove = true;
                            self.confirm = None;
                        } else {
                            self.confirm = Some(key);
                        }
                    }
                    if let (Origin::Installed, Some(url)) = (report.origin, &report.url) {
                        ui.add_enabled_ui(!self.installing, |ui| {
                            if widgets::button(ui, None, "Update").clicked() {
                                update = Some(url.clone());
                            }
                        });
                    }
                });
            });
        });
        if remove {
            self.ask(
                Ask::Do(format!("remove {}", report.name)),
                Request::Remove {
                    id: report.id.clone(),
                },
            );
            self.view = View::Overview;
        }
        if let Some(url) = update {
            self.install(&ctx, url);
        }
    }

    fn add(&mut self, ui: &mut Ui) {
        widgets::page_header(
            ui,
            glyph::ADD,
            "Add a plugin",
            "From GitHub, a zip link, or a folder on this computer.",
            |_| {},
        );
        let mut go = false;
        widgets::card(ui, None, |ui| {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 110.0).max(160.0);
                let field = egui::TextEdit::singleline(&mut self.add_text)
                    .hint_text("owner/repo, a link, or a folder path")
                    .font(theme::body(14.0))
                    .text_color(theme::TEXT)
                    .desired_width(width)
                    .frame(
                        Frame::new()
                            .fill(theme::FIELD)
                            .corner_radius(theme::RADIUS_CONTROL)
                            .inner_margin(Margin::symmetric(10, 7)),
                    );
                let response = ui.add_enabled(!self.installing, field);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.installing {
                        ui.add(egui::Spinner::new().size(16.0).color(theme::MUTED));
                        widgets::hint(ui, "Installing");
                    } else {
                        let ready = !self.add_text.trim().is_empty();
                        ui.add_enabled_ui(ready, |ui| {
                            if widgets::button(ui, Some(glyph::ADD), "Add").clicked() {
                                go = true;
                            }
                        });
                    }
                });
            });
            ui.add_space(6.0);
            widgets::hint(
                ui,
                "A folder is linked in place and reloads on every save. Anything else is downloaded.",
            );
        });
        if go {
            self.submit(ui.ctx());
        }
        ui.add_space(12.0);

        widgets::card(ui, Some("Write one"), |ui| {
            widgets::hint(
                ui,
                "A plugin is a folder with a plugin.toml and an init.lua. The built-in ones are the examples:",
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for name in [
                    "night-light",
                    "display-profiles",
                    "wallpaper-pause",
                    "shortcuts",
                ] {
                    widgets::chip(ui, None, name);
                }
            });
        });
    }

    fn submit(&mut self, ctx: &egui::Context) {
        let text = self.add_text.trim().to_owned();
        if text.is_empty() || self.installing {
            return;
        }
        let path = Path::new(&text);
        if path.is_dir() {
            self.open_new = Some(self.entries.iter().map(|e| e.report.id.clone()).collect());
            self.ask(
                Ask::Do("link that folder".to_owned()),
                Request::Link {
                    path: path.to_string_lossy().into_owned(),
                },
            );
            self.add_text.clear();
        } else {
            self.install(ctx, text);
        }
    }

    fn settings(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        widgets::page_header(
            ui,
            glyph::SETTINGS,
            "Settings",
            &format!("cortisoff {}", env!("CARGO_PKG_VERSION")),
            |_| {},
        );

        widgets::card(ui, Some("Startup"), |ui| {
            let mut on = self.at_logon.unwrap_or(false);
            let mut changed = false;
            widgets::row(ui, "Start when I log in", None, |ui| {
                changed = widgets::switch(ui, &mut on).changed();
            });
            if changed {
                match cortisoff_daemon::start_at_logon(on) {
                    Ok(()) => self.at_logon = Some(on),
                    Err(e) => self.say(format!("Start at login: {e}"), true),
                }
            }
        });
        ui.add_space(12.0);

        self.updates(ui, &ctx);
        ui.add_space(12.0);

        if let Some(doctor) = self.doctor.clone() {
            widgets::card(ui, Some("What this machine can do"), |ui| {
                for (n, cap) in doctor.capabilities.iter().enumerate() {
                    if n > 0 {
                        widgets::divider(ui);
                    }
                    let hint = match (&cap.missing_because, &cap.backend) {
                        (Some(why), _) => Some(why.clone()),
                        (None, Some(backend)) => Some(backend.clone()),
                        (None, None) => None,
                    };
                    widgets::row(ui, &cap.label, hint.as_deref(), |ui| {
                        let (text, colour) = if cap.is_available() {
                            ("Ready", theme::HOLDING)
                        } else {
                            ("Missing", theme::SUBTLE)
                        };
                        ui.label(
                            RichText::new(text)
                                .font(theme::body(13.0))
                                .color(theme::MUTED),
                        );
                        widgets::dot(ui, colour);
                    });
                }
                for warning in &doctor.warnings {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(warning)
                            .font(theme::body(12.5))
                            .color(theme::PAUSED),
                    );
                }
            });
            ui.add_space(12.0);
        }

        widgets::card(ui, Some("cortisoff"), |ui| {
            let running = self.daemon == Daemon::Up;
            let hint = match (running, &self.doctor) {
                (true, Some(d)) => format!("Running, process {}", d.pid),
                (true, None) => "Running".to_owned(),
                (false, _) => "Not running".to_owned(),
            };
            widgets::row(ui, "Background process", Some(&hint), |ui| {
                if running {
                    if widgets::button(ui, Some(glyph::PAUSE), "Stop").clicked() {
                        self.stop_daemon();
                    }
                } else if widgets::button(ui, Some(glyph::ARROW), "Start").clicked() {
                    self.start_daemon();
                }
            });
            if installed() {
                widgets::divider(ui);
                let key = "uninstall";
                widgets::row(
                    ui,
                    "Uninstall",
                    Some("Removes the app. Your plugins and settings stay."),
                    |ui| {
                        let label = if self.confirm == Some(key) {
                            "Click again to confirm"
                        } else {
                            "Uninstall"
                        };
                        if widgets::button(ui, Some(glyph::CLOSE), label).clicked() {
                            if self.confirm == Some(key) {
                                self.confirm = None;
                                self.spawn(&ctx, || Done::Uninstalled(crate::uninstall()));
                            } else {
                                self.confirm = Some(key);
                            }
                        }
                    },
                );
            }
        });
    }

    fn updates(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let installed = installed();
        let mut act: Option<u8> = None;
        widgets::card(ui, Some("Updates"), |ui| {
            let (line, hint): (String, Option<String>) = match &self.update {
                Update::Idle => ("Not checked yet".to_owned(), None),
                Update::Checking => ("Checking".to_owned(), None),
                Update::Current => ("Up to date".to_owned(), None),
                Update::Available(r) => (
                    format!("Version {} is out", r.version),
                    Some(first_line(&r.notes)).filter(|n| !n.is_empty()),
                ),
                Update::Downloading(v) => (format!("Downloading {v}"), None),
                Update::Ready { version, .. } => (format!("Version {version} is ready"), None),
                Update::Restarting => ("Restarting".to_owned(), None),
                Update::Failed(e) => ("Update failed".to_owned(), Some(e.clone())),
            };
            let hint = hint.or_else(|| {
                (!installed).then(|| "Updates install over the installed copy only.".to_owned())
            });
            widgets::row(ui, &line, hint.as_deref(), |ui| match &self.update {
                Update::Checking | Update::Downloading(_) | Update::Restarting => {
                    ui.add(egui::Spinner::new().size(16.0).color(theme::MUTED));
                }
                Update::Available(_) if installed => {
                    if widgets::button(ui, None, "Download").clicked() {
                        act = Some(1);
                    }
                }
                Update::Ready { .. } => {
                    if widgets::button(ui, Some(glyph::ARROW), "Restart now").clicked() {
                        act = Some(2);
                    }
                }
                _ => {
                    if widgets::button(ui, None, "Check").clicked() {
                        act = Some(0);
                    }
                }
            });
        });
        match act {
            Some(0) => {
                self.update = Update::Checking;
                self.spawn(ctx, || {
                    Done::Checked(cortisoff_update::check(
                        cortisoff_update::FEED_URL,
                        env!("CARGO_PKG_VERSION"),
                    ))
                });
            }
            Some(1) => {
                if let Update::Available(release) = &self.update {
                    let release = release.clone();
                    self.update = Update::Downloading(release.version.clone());
                    self.spawn(ctx, move || {
                        let dir = std::env::temp_dir().join("cortisoff-update");
                        Done::Downloaded(
                            cortisoff_update::download(&release, &dir)
                                .map(|staged| (staged, release.version.clone())),
                        )
                    });
                }
            }
            Some(2) => {
                if let Update::Ready { staged, .. } = &self.update {
                    let staged = staged.clone();
                    self.update = Update::Restarting;
                    self.spawn(ctx, move || Done::Restarted(crate::restart_into(&staged)));
                }
            }
            _ => {}
        }
    }

    fn notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = &self.notice else { return };
        if notice.at.elapsed() > NOTICE_FOR {
            self.notice = None;
            return;
        }
        ctx.request_repaint_after(NOTICE_FOR - notice.at.elapsed());
        let mut close = false;
        egui::Area::new(egui::Id::new("notice"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -20.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(theme::FIELD)
                    .stroke(egui::Stroke::new(
                        1.0,
                        if notice.error {
                            theme::FAILED
                        } else {
                            theme::CARD_BORDER
                        },
                    ))
                    .corner_radius(theme::RADIUS_CARD)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.horizontal(|ui| {
                            widgets::dot(
                                ui,
                                if notice.error {
                                    theme::FAILED
                                } else {
                                    theme::HOLDING
                                },
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&notice.text)
                                        .font(theme::body(13.0))
                                        .color(theme::TEXT),
                                )
                                .wrap(),
                            );
                            let x = ui.add(
                                egui::Label::new(
                                    RichText::new(glyph::CLOSE)
                                        .font(theme::icon(10.0))
                                        .color(theme::MUTED),
                                )
                                .sense(egui::Sense::click()),
                            );
                            close = x.clicked();
                        });
                    });
            });
        if close {
            self.notice = None;
        }
    }

    fn cap_label(&self, cap: &CapabilityId) -> String {
        self.doctor
            .as_ref()
            .and_then(|d| d.capabilities.iter().find(|c| &c.id == cap))
            .map(|c| c.label.clone())
            .unwrap_or_else(|| cap.to_string())
    }

    fn owner_name(&self, owner: &str) -> String {
        match owner {
            "manual" => "set by hand".to_owned(),
            "preview" => "preview".to_owned(),
            id => self
                .entries
                .iter()
                .find(|e| e.report.id == id || e.report.name == id)
                .map(|e| e.report.name.clone())
                .unwrap_or_else(|| id.to_owned()),
        }
    }
}

/// A note above a plugin's page: why it cannot run, or what it can do.
fn note(ui: &mut Ui, colour: Color32, text: &str) {
    Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0, theme::CARD_BORDER))
        .corner_radius(theme::RADIUS_CARD)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                widgets::dot(ui, colour);
                ui.add(
                    egui::Label::new(
                        RichText::new(text)
                            .font(theme::body(13.0))
                            .color(theme::TEXT),
                    )
                    .wrap(),
                );
            });
        });
    ui.add_space(10.0);
}

fn world_chips(ui: &mut Ui, world: &WorldReport) {
    if let Some(app) = &world.app {
        widgets::chip(ui, Some(glyph::GAME), &format!("{} in front", app.exe));
    }
    if world.fullscreen || world.maximized {
        widgets::chip(ui, Some(glyph::FULLSCREEN), "fills the screen");
    }
    if world.audio_active {
        widgets::chip(ui, Some(glyph::VOLUME), "sound playing");
    }
    if world.idle {
        widgets::chip(ui, Some(glyph::PAUSE), "idle");
    }
    if world.locked {
        widgets::chip(ui, Some(glyph::LOCK), "locked");
    }
    if world.on_battery {
        widgets::chip(ui, Some(glyph::BOLT), "on battery");
    }
    if let Some(m) = world.minute_of_day {
        widgets::chip(
            ui,
            Some(glyph::CLOCK),
            &format!("{:02}:{:02}", m / 60, m % 60),
        );
    }
}

fn color(state: State) -> Color32 {
    match state {
        State::Holding => theme::HOLDING,
        State::Waiting => theme::WAITING,
        State::Paused => theme::PAUSED,
        State::Failed => theme::FAILED,
    }
}

fn show_value(cap: &CapabilityId, value: &Value) -> String {
    let id = cap.as_str();
    match value {
        Value::Int(n) if id.ends_with("color_temp") => format!("{n} K"),
        Value::Int(n) if id.ends_with("vibrance") || id.ends_with("brightness") => {
            format!("{n} %")
        }
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format!("{f:.2}"),
        Value::Bool(b) => if *b { "on" } else { "off" }.to_owned(),
        Value::Text(t) => t.clone(),
        Value::Mode {
            width,
            height,
            refresh_mhz,
        } => {
            let hz = f64::from(*refresh_mhz) / 1000.0;
            if refresh_mhz % 1000 == 0 {
                format!("{width}x{height}, {hz:.0} Hz")
            } else {
                format!("{width}x{height}, {hz:.2} Hz")
            }
        }
    }
}

fn list(platforms: &[String]) -> String {
    let names: Vec<&str> = platforms
        .iter()
        .map(|p| match p.as_str() {
            "windows" => "Windows",
            "linux" => "Linux",
            "macos" => "macOS",
            other => other,
        })
        .collect();
    match names.as_slice() {
        [] => "no system".to_owned(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn plugins_dir() -> Option<PathBuf> {
    cortisoff_daemon::Paths::from_env().ok().map(|p| p.plugins)
}

/// Whether this is the installed copy, the only one an update replaces.
fn installed() -> bool {
    std::env::current_exe()
        .map(|exe| cortisoff_update::install::is_installed(&exe))
        .unwrap_or(false)
}

/// Whether a start or a stop asked at `t` is still on its way.
fn recent(t: Option<Instant>) -> bool {
    t.is_some_and(|t| t.elapsed() < IN_TRANSIT)
}

/// Time left in the current minute of the local clock, plus a little so the
/// frame lands after the turn rather than just before it.
fn until_next_minute() -> Duration {
    let (_, _, _, _, _, s, ms) = local_time();
    let into = u64::from(s.min(59)) * 1000 + u64::from(ms.min(999));
    Duration::from_millis(60_000 - into + 20)
}

/// The logo, which is also cortisoff's own switch: the gauge and the name,
/// a green "cortisoff" while it runs and a red "cortison" while it does not.
/// The needle swings from calm to stressed as it goes off. Grey and calm
/// before the daemon has answered.
fn switch(ui: &mut Ui, on: Option<bool>, busy: bool) -> egui::Response {
    let t =
        ui.ctx()
            .animate_bool_with_time(egui::Id::new("cortisoff-switch"), on.unwrap_or(true), 0.3);
    let colour = match on {
        None => theme::SUBTLE,
        Some(_) => theme::FAILED.lerp_to_gamma(theme::HOLDING, t),
    };
    let word = if t >= 0.5 { "cortisoff" } else { "cortison" };
    // The name dims through the swap instead of jumping from one to the other.
    let ink = colour.gamma_multiply(0.35 + 0.65 * (2.0 * t - 1.0).abs());
    let galley = ui
        .painter()
        .layout_no_wrap(word.to_owned(), theme::strong(16.0), ink);
    let size = vec2(14.0 + 22.0 + 8.0 + galley.size().x + 18.0, 38.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on == Some(true), word)
    });

    // A pill outlined in the state's colour, so it reads as something to
    // press rather than a title. Brighter under the pointer, brighter again
    // while held.
    let live = !busy && on.is_some();
    let (fill, edge) = if live && response.is_pointer_button_down_on() {
        (0.26, 1.0)
    } else if live && response.hovered() {
        (0.18, 0.9)
    } else {
        (0.09, 0.5)
    };
    let painter = ui.painter();
    let pill = egui::CornerRadius::same(19);
    painter.rect(
        rect.shrink(0.75),
        pill,
        colour.gamma_multiply(fill),
        egui::Stroke::new(1.5, colour.gamma_multiply(edge)),
        egui::StrokeKind::Inside,
    );
    let c = pos2(rect.left() + 25.0, rect.center().y + 3.0);
    let arc: Vec<_> = (0..=24)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 24.0);
            pos2(c.x + 9.0 * a.cos(), c.y + 9.0 * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(arc, egui::Stroke::new(2.5, colour)));
    let a = std::f32::consts::PI * (1.75 - 0.5 * t);
    painter.line_segment(
        [c, c + 7.0 * vec2(a.cos(), a.sin())],
        egui::Stroke::new(2.0, theme::TEXT),
    );
    painter.galley(
        pos2(rect.left() + 44.0, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );

    let tip = match (on, busy) {
        (None, _) => "Looking for cortisoff",
        (Some(true), true) => "Starting",
        (Some(false), true) => "Stopping",
        (Some(true), false) => "Running. Click to stop every plugin.",
        (Some(false), false) => "Stopped. Click to start.",
    };
    let response = if !live {
        response
    } else {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    };
    response.on_hover_text(tip)
}

pub(crate) fn local_time() -> (u16, u8, u8, u8, u8, u8, u16) {
    #[cfg(windows)]
    {
        cortisoff_win::local_time()
    }
    #[cfg(unix)]
    {
        cortisoff_unix::local_time()
    }
}

// ---- captures ------------------------------------------------------------

impl Shell {
    /// A window with the built-in plugins and a made-up machine, no daemon,
    /// opened on `view`. What the captures render.
    pub fn demo(view: View) -> Shell {
        let mut shell = Shell::with(None, true);
        shell.view = view;
        shell.daemon = Daemon::Up;
        let states = [
            (
                "night-light",
                true,
                serde_json::json!({ "kelvin": 3400, "holding": true }),
            ),
            (
                "display-profiles",
                true,
                serde_json::json!({
                    "app": "cs2.exe", "profile": "cs2.exe",
                    "held": ["vibrance 80 %"], "problems": [],
                    "can_mode": true, "can_vibrance": true
                }),
            ),
            (
                "wallpaper-pause",
                true,
                serde_json::json!({ "paused": true, "reason": "an app fills the screen" }),
            ),
            (
                "shortcuts",
                false,
                serde_json::json!({ "results": [{ "ok": true }, { "ok": true }, { "ok": true }] }),
            ),
        ];
        for package in cortisoff_runtime::builtin() {
            let m = &package.manifest;
            let (enabled, state) = states
                .iter()
                .find(|(id, _, _)| *id == m.id)
                .map(|(_, on, st)| (*on, st.clone()))
                .unwrap_or((false, serde_json::Value::Null));
            shell.adopt(PluginReport {
                id: m.id.clone(),
                name: m.name.clone(),
                version: m.version.clone(),
                description: m.description.clone(),
                icon: m.icon.clone(),
                author: m.author.clone(),
                platforms: m.platforms.clone(),
                supported: true,
                origin: Origin::Builtin,
                dir: None,
                url: None,
                ffi: m.ffi,
                enabled,
                running: enabled,
                error: None,
                settings: serde_json::Value::Null,
                state,
                holds: Vec::new(),
            });
        }
        let cap = |id: &str| CapabilityId::new(id);
        shell.state = Some(StateReport {
            active: true,
            world: WorldReport {
                app: Some(cortisoff_host::AppRef {
                    exe: "cs2.exe".into(),
                    title: None,
                    pid: 4120,
                    started_at_ms: 0,
                }),
                fullscreen: true,
                maximized: true,
                minute_of_day: Some(21 * 60 + 12),
                ..WorldReport::default()
            },
            plugins: Vec::new(),
            claims: vec![
                ClaimReport {
                    cap: cap("display.color_temp"),
                    owner: "night-light".into(),
                    priority: 0,
                    value: Value::Int(3400),
                    shadowed: Vec::new(),
                },
                ClaimReport {
                    cap: cap("display.vibrance"),
                    owner: "display-profiles".into(),
                    priority: 100,
                    value: Value::Int(80),
                    shadowed: Vec::new(),
                },
                ClaimReport {
                    cap: cap("wallpaper.playback"),
                    owner: "wallpaper-pause".into(),
                    priority: 0,
                    value: Value::Bool(false),
                    shadowed: Vec::new(),
                },
            ],
        });
        for (time, text) in [
            (
                "21:12",
                "Wallpaper pause paused the wallpaper: an app fills the screen",
            ),
            ("21:12", "Display profiles holds vibrance 80 % for cs2.exe"),
            ("20:30", "Night light reached 3400 K"),
            ("20:00", "Night light started warming the screen"),
        ] {
            shell.recent.push_back(LogLine {
                time: time.into(),
                level: LogLevel::Info,
                text: text.into(),
            });
        }
        let report = |id: &str, label: &str, backend: Option<&str>, missing: Option<&str>| {
            cortisoff_ipc::CapabilityReport {
                id: cap(id),
                backend: backend.map(str::to_owned),
                missing_because: missing.map(str::to_owned),
                settable: true,
                label: label.to_owned(),
                control: None,
            }
        };
        shell.doctor = Some(DoctorReport {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: 4242,
            capabilities: vec![
                report(
                    "display.color_temp",
                    "Colour temperature",
                    Some("gamma ramp"),
                    None,
                ),
                report("display.vibrance", "Vibrance", Some("nvapi"), None),
                report(
                    "display.mode",
                    "Resolution and refresh rate",
                    Some("ccd"),
                    None,
                ),
                report(
                    "wallpaper.playback",
                    "Wallpaper playback",
                    None,
                    Some("neither Wallpaper Engine nor Lively is installed or running"),
                ),
            ],
            ..DoctorReport::default()
        });
        shell.at_logon = Some(true);
        shell.update = Update::Current;
        shell
    }

    /// The same, as it looks once the daemon has gone away.
    pub fn stopped(mut self) -> Shell {
        self.daemon = Daemon::Down;
        self.state = None;
        self.recent.clear();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_follows_the_daemon_and_turns_off_at_once() {
        let mut shell = Shell::with(None, true);
        assert_eq!(shell.switched_on(), None);
        shell.daemon = Daemon::Up;
        assert_eq!(shell.switched_on(), Some(true));

        // A stop shows before the daemon has gone, and a second click while
        // it goes does nothing rather than starting a new one.
        shell.toggle();
        assert_eq!(shell.switched_on(), Some(false));
        shell.toggle();
        assert_eq!(shell.switched_on(), Some(false));
        assert!(shell.starting.is_none());

        // A stop the daemon never acted on gives way to what it says.
        if let Some(long_ago) = Instant::now().checked_sub(IN_TRANSIT * 2) {
            shell.stopping = Some(long_ago);
            assert_eq!(shell.switched_on(), Some(true));
        }

        shell.daemon = Daemon::Down;
        shell.stopping = None;
        assert_eq!(shell.switched_on(), Some(false));
    }
}
