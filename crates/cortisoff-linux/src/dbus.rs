//! The D-Bus half of the host: the buses, and the threads that turn signals
//! into wakeups of the host's loop.
//!
//! Wayland gives a client no protocol for the session lock, the idle time or
//! the colour of the screen; the desktop answers those over D-Bus. zbus speaks
//! it in Rust over the socket, with its blocking API and its own small
//! executor, so no C library and no async runtime come with it.
//!
//! Signals are read on one thread per subscription, each parked in the bus
//! socket until a message arrives. A thread pushes what it read into the
//! [`Inbox`] and calls the host's waker, which is how a lock or an idle
//! threshold ends a blocked `poll` at once. Nothing here looks on a beat.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use cortisoff_host::Waker;
use zbus::blocking::proxy::Builder;
use zbus::blocking::{Connection, Proxy};
use zbus::message::Message;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// What a listener thread hands the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// logind's `LockedHint` on the graphical session moved.
    Locked(bool),
    /// Mutter fired an idle or user-active watch.
    IdleWatch(u32),
    /// timedated announced a new time zone.
    Zone,
}

/// The queue between the listener threads and the host's loop.
#[derive(Clone)]
pub struct Inbox {
    queue: Arc<Mutex<Vec<Signal>>>,
    waker: Waker,
}

impl Inbox {
    pub fn new(waker: Waker) -> Self {
        Self {
            queue: Arc::new(Mutex::new(Vec::new())),
            waker,
        }
    }

    fn push(&self, signal: Signal) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(signal);
        (self.waker)();
    }

    pub fn drain(&self) -> Vec<Signal> {
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The session and system buses, each with the reason it is absent.
pub struct Buses {
    pub session: Result<Connection, String>,
    pub system: Result<Connection, String>,
    pub inbox: Inbox,
}

impl Buses {
    pub fn connect(waker: Waker) -> Self {
        Self {
            session: Connection::session()
                .map_err(|e| format!("the session bus is not reachable: {e}")),
            system: Connection::system()
                .map_err(|e| format!("the system bus is not reachable: {e}")),
            inbox: Inbox::new(waker),
        }
    }
}

/// A proxy that never caches properties: every read asks the service, and
/// building one sends nothing on the bus.
pub fn proxy(
    conn: &Connection,
    destination: &'static str,
    path: &str,
    interface: &'static str,
) -> Result<Proxy<'static>, String> {
    Builder::<Proxy<'static>>::new(conn)
        .destination(destination)
        .and_then(|b| b.path(path.to_owned()))
        .and_then(|b| b.interface(interface))
        .map(|b| b.cache_properties(CacheProperties::No))
        .and_then(Builder::build)
        .map_err(|e| format!("{destination} {path}: {e}"))
}

/// Whether some process owns `name` on this bus right now.
pub fn has_owner(conn: &Connection, name: &'static str) -> bool {
    proxy(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .and_then(|p| {
        p.call::<_, _, bool>("NameHasOwner", &(name,))
            .map_err(|e| e.to_string())
    })
    .unwrap_or(false)
}

/// Subscribe to `member` on `proxy` now, then read it on a thread of its own
/// until the bus goes away. The match rule is in place when this returns, so
/// a signal sent right after cannot be missed.
pub fn listen(
    proxy: &Proxy<'static>,
    member: &'static str,
    inbox: &Inbox,
    read: impl Fn(&Message) -> Option<Signal> + Send + 'static,
) -> Result<(), String> {
    let signals = proxy
        .receive_signal(member)
        .map_err(|e| format!("could not subscribe to {member}: {e}"))?;
    let inbox = inbox.clone();
    std::thread::Builder::new()
        .name(format!("cortisoff-dbus-{member}"))
        .spawn(move || {
            for message in signals {
                if let Some(signal) = read(&message) {
                    inbox.push(signal);
                }
            }
        })
        .map(|_| ())
        .map_err(|e| format!("could not start the {member} thread: {e}"))
}

/// The body of `org.freedesktop.DBus.Properties.PropertiesChanged`.
type Changed = (String, HashMap<String, OwnedValue>, Vec<String>);

/// A boolean property's new value from a `PropertiesChanged`, if this one
/// carries it.
fn changed_bool(message: &Message, interface: &str, property: &str) -> Option<bool> {
    let (iface, changed, _) = message.body().deserialize::<Changed>().ok()?;
    if iface != interface {
        return None;
    }
    changed
        .get(property)
        .and_then(|v| bool::try_from(v.clone()).ok())
}

// ---- logind --------------------------------------------------------------

const LOGIN1: &str = "org.freedesktop.login1";
const SESSION: &str = "org.freedesktop.login1.Session";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The session lock, from logind.
///
/// `LockedHint` is the session's lock state as the desktop reports it: GNOME
/// Shell, KDE and the screen lockers set it when the screen locks and clear it
/// on unlock. logind's `Lock` and `Unlock` signals are requests to lock, sent
/// by `loginctl lock-session`, and say nothing about whether a locker obeyed,
/// so they are not read as state. The same path serves X11 and Wayland.
pub struct Logind {
    session: Proxy<'static>,
}

impl Logind {
    pub fn new(buses: &Buses) -> Result<Self, String> {
        let system = buses.system.as_ref().map_err(Clone::clone)?;
        let path = graphical_session(system)?;
        let session = proxy(system, LOGIN1, &path, SESSION)?;
        let props = proxy(system, LOGIN1, &path, PROPERTIES)?;
        listen(&props, "PropertiesChanged", &buses.inbox, |m| {
            changed_bool(m, SESSION, "LockedHint").map(Signal::Locked)
        })?;
        let me = Self { session };
        // Read once so a session without the property is refused here.
        me.locked()?;
        Ok(me)
    }

    pub fn locked(&self) -> Result<bool, String> {
        self.session
            .get_property::<bool>("LockedHint")
            .map_err(|e| format!("logind would not say whether the session is locked: {e}"))
    }
}

/// The session the user's desktop runs in. Not the caller's own: a daemon
/// started from a terminal over ssh sits in that ssh session, which never
/// locks. logind's `Display` property on the user names the graphical one;
/// `XDG_SESSION_ID` is the fallback for a desktop logind does not mark.
fn graphical_session(system: &Connection) -> Result<String, String> {
    let manager = proxy(
        system,
        LOGIN1,
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    let uid = rustix::process::getuid().as_raw();
    let user: Option<OwnedObjectPath> = manager.call("GetUser", &(uid,)).ok();
    if let Some(user) = user {
        let display = proxy(system, LOGIN1, user.as_str(), "org.freedesktop.login1.User")
            .ok()
            .and_then(|p| p.get_property::<(String, OwnedObjectPath)>("Display").ok());
        if let Some((_, path)) = display.filter(|(_, p)| p.as_str() != "/") {
            return Ok(path.as_str().to_owned());
        }
    }
    if let Ok(id) = std::env::var("XDG_SESSION_ID") {
        if let Ok(path) = manager.call::<_, _, OwnedObjectPath>("GetSession", &(id.as_str(),)) {
            return Ok(path.as_str().to_owned());
        }
    }
    Err(
        "logind knows no graphical session for this user, so there is nothing whose lock to follow"
            .to_owned(),
    )
}

// ---- timedated -----------------------------------------------------------

/// Hear the time zone change. timedated starts on demand and exits when idle,
/// but the match rule waits for it on the bus all the same.
pub fn watch_zone(buses: &Buses) -> Result<(), String> {
    let system = buses.system.as_ref().map_err(Clone::clone)?;
    let props = proxy(
        system,
        "org.freedesktop.timedate1",
        "/org/freedesktop/timedate1",
        PROPERTIES,
    )?;
    listen(&props, "PropertiesChanged", &buses.inbox, |m| {
        let (iface, changed, invalidated) = m.body().deserialize::<Changed>().ok()?;
        let zone = changed.contains_key("Timezone") || invalidated.iter().any(|p| p == "Timezone");
        (iface == "org.freedesktop.timedate1" && zone).then_some(Signal::Zone)
    })
}

// ---- Mutter --------------------------------------------------------------

const IDLE_MONITOR: &str = "org.gnome.Mutter.IdleMonitor";

/// GNOME's idle time, from the compositor that sees every input.
///
/// Event based: an idle watch fires once when the idle time crosses its
/// threshold, and a user-active watch fires once at the first input after
/// that, then removes itself. Watches belong to this connection, so Mutter
/// drops them when the daemon exits.
pub struct IdleMonitor {
    proxy: Proxy<'static>,
    /// Watch id and the threshold in seconds it stands for.
    thresholds: Vec<(u32, u32)>,
    active: Option<u32>,
}

impl IdleMonitor {
    pub fn new(buses: &Buses) -> Result<Self, String> {
        let session = buses.session.as_ref().map_err(Clone::clone)?;
        if !has_owner(session, IDLE_MONITOR) {
            return Err("org.gnome.Mutter.IdleMonitor is not on the session bus: the compositor is not Mutter".to_owned());
        }
        let proxy = proxy(
            session,
            IDLE_MONITOR,
            "/org/gnome/Mutter/IdleMonitor/Core",
            IDLE_MONITOR,
        )?;
        listen(&proxy, "WatchFired", &buses.inbox, |m| {
            m.body().deserialize::<u32>().ok().map(Signal::IdleWatch)
        })?;
        let me = Self {
            proxy,
            thresholds: Vec::new(),
            active: None,
        };
        me.idle_ms()
            .ok_or("Mutter would not report the idle time")?;
        Ok(me)
    }

    pub fn idle_ms(&self) -> Option<u64> {
        self.proxy.call::<_, _, u64>("GetIdletime", &()).ok()
    }

    /// One idle watch per threshold, replacing the previous set.
    pub fn watch(&mut self, seconds: &[u32]) {
        let wanted: Vec<u32> = seconds.iter().copied().filter(|s| *s > 0).collect();
        if self
            .thresholds
            .iter()
            .map(|(_, s)| *s)
            .eq(wanted.iter().copied())
        {
            return;
        }
        self.unwatch();
        for s in wanted {
            let ms = u64::from(s) * 1000;
            if let Ok(id) = self.proxy.call::<_, _, u32>("AddIdleWatch", &(ms,)) {
                self.thresholds.push((id, s));
            }
        }
    }

    pub fn unwatch(&mut self) {
        for (id, _) in self.thresholds.drain(..) {
            let _ = self.proxy.call::<_, _, ()>("RemoveWatch", &(id,));
        }
    }

    /// Hold a user-active watch while `on`, none otherwise. Returns whether
    /// the return of the user will be announced.
    pub fn listen_for_return(&mut self, on: bool) -> bool {
        match (on, self.active) {
            (true, None) => {
                self.active = self.proxy.call::<_, _, u32>("AddUserActiveWatch", &()).ok();
            }
            (false, Some(id)) => {
                let _ = self.proxy.call::<_, _, ()>("RemoveWatch", &(id,));
                self.active = None;
            }
            _ => {}
        }
        self.active.is_some()
    }

    /// A watch fired. True when it is one of ours. A user-active watch is
    /// gone once it fired.
    pub fn fired(&mut self, id: u32) -> bool {
        if self.active == Some(id) {
            self.active = None;
            return true;
        }
        self.thresholds.iter().any(|(w, _)| *w == id)
    }
}
