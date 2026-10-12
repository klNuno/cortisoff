//! Global shortcuts under Wayland, through the desktop portal.
//!
//! A Wayland client cannot grab a key: only the compositor sees every key
//! press. The `org.freedesktop.portal.GlobalShortcuts` interface is how an
//! application asks for one anyway. It opens a session, binds a list of
//! shortcuts (GNOME shows a dialog where the user allows them, the first time
//! for each), and sends `Activated` with the shortcut's id when one is
//! pressed. GNOME 48 and later, and KDE Plasma, implement it.
//!
//! Each chord is one shortcut, and its id is the chord itself (`ctrl+alt+k`),
//! not the engine's hotkey id. The desktop remembers what the user allowed per
//! id: a hotkey id is renumbered at every start and would come back attached
//! to whatever chord had it before, while the chord as id always means the
//! same keys, and a chord the user allowed once is never asked about again.
//!
//! A portal session binds once. A new set of hotkeys closes the old session
//! and binds the set on a new one. Binding waits for the user's answer in the
//! dialog, so it runs on a thread: the host waits a moment for an answer that
//! needs no dialog, and an answer that comes later arrives through the inbox
//! like every other signal.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cortisoff_host::{Chord, Hotkey, Key, Named};
use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::dbus::{self, Buses, Inbox, Signal};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";

/// How long `set` waits for the portal before answering. Long enough for a
/// set the user already allowed, which needs no dialog; a dialog answer
/// comes later through the inbox.
const QUICK_ANSWER: Duration = Duration::from_millis(1_500);

pub const DECLINED: &str = "the shortcut dialog was closed without allowing it. Switch the plugin off and on to be asked again";

/// The outcome of one bind, per chord: the ones the desktop did not give.
pub type Outcome = Result<Vec<(String, String)>, String>;

/// What the listener thread and the bind threads share with the host.
#[derive(Default)]
struct Shared {
    /// Counts binds. A bind that finishes after a newer one started closes
    /// its session instead of keeping it.
    generation: u64,
    /// The session whose shortcuts are live.
    session: Option<OwnedObjectPath>,
}

pub struct Shortcuts {
    conn: Connection,
    inbox: Inbox,
    shared: Arc<Mutex<Shared>>,
    /// Chord id to the engine's hotkey id, for the live set.
    live: HashMap<String, u32>,
    /// Chords whose dialog the user closed during this run. Asking again at
    /// every settings change would put the dialog back in their face.
    declined: HashSet<String>,
    /// The generation whose outcome `set` already reported.
    answered: u64,
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shortcuts {
    pub fn new(buses: &Buses) -> Result<Self, String> {
        let conn = buses.session.as_ref().map_err(Clone::clone)?.clone();
        // The portal starts on demand, so it is asked rather than looked for.
        let portal = dbus::proxy(&conn, PORTAL, DESKTOP, SHORTCUTS)?;
        portal.get_property::<u32>("version").map_err(|e| {
            format!("the desktop portal offers no GlobalShortcuts interface (GNOME has it from version 48, KDE Plasma from 5.27): {e}")
        })?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let mine = Arc::clone(&shared);
        dbus::listen(&portal, "Activated", &buses.inbox, move |m| {
            let (session, id, _, _) = m
                .body()
                .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
                .ok()?;
            let live = lock(&mine).session.as_ref() == Some(&session);
            live.then_some(Signal::Shortcut(id))
        })?;
        Ok(Self {
            conn,
            inbox: buses.inbox.clone(),
            shared,
            live: HashMap::new(),
            declined: HashSet::new(),
            answered: 0,
        })
    }

    /// The engine's hotkey for a pressed shortcut.
    pub fn hotkey(&self, shortcut: &str) -> Option<u32> {
        self.live.get(shortcut).copied()
    }

    /// Replace the live set. Returns the hotkeys refused now; a refusal that
    /// needs the user's answer arrives later as [`Signal::Bound`].
    pub fn set(&mut self, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        let mut refused = Vec::new();
        let mut bind = Vec::new();
        self.live.clear();
        for hotkey in wanted {
            let id = hotkey.chord.to_string();
            let Some(trigger) = trigger(&hotkey.chord) else {
                refused.push((
                    hotkey.id,
                    format!("{id}: the GlobalShortcuts portal has no name for this key"),
                ));
                continue;
            };
            if self.declined.contains(&id) {
                refused.push((hotkey.id, format!("{id}: {DECLINED}")));
                continue;
            }
            if self.live.contains_key(&id) {
                refused.push((hotkey.id, format!("{id} is already a shortcut of another plugin")));
                continue;
            }
            self.live.insert(id.clone(), hotkey.id);
            bind.push((id, trigger));
        }

        let (generation, old) = {
            let mut s = lock(&self.shared);
            s.generation += 1;
            (s.generation, s.session.take())
        };
        if let Some(old) = old {
            close(&self.conn, &old);
        }
        if bind.is_empty() {
            return refused;
        }

        let (tx, rx) = mpsc::channel();
        let conn = self.conn.clone();
        let shared = Arc::clone(&self.shared);
        let inbox = self.inbox.clone();
        let spawned = std::thread::Builder::new()
            .name("cortisoff-shortcuts".into())
            .spawn(move || {
                let outcome = match bind_all(&conn, &bind) {
                    Ok((session, missing)) => {
                        let mut s = lock(&shared);
                        if s.generation == generation {
                            s.session = Some(session);
                        } else {
                            drop(s);
                            close(&conn, &session);
                        }
                        Ok(missing)
                    }
                    Err(e) => Err(e),
                };
                let _ = tx.send(outcome.clone());
                inbox.push(Signal::Bound(generation, outcome));
            });
        if let Err(e) = spawned {
            let why = format!("could not start the shortcut thread: {e}");
            return wanted.iter().map(|h| (h.id, why.clone())).collect();
        }
        if let Ok(outcome) = rx.recv_timeout(QUICK_ANSWER) {
            self.answered = generation;
            refused.extend(self.refusals(outcome));
        }
        refused
    }

    /// A bind finished after `set` returned. The refusals it carries can no
    /// longer reach the plugin, so they are returned for the log.
    pub fn bound_late(&mut self, generation: u64, outcome: Outcome) -> Vec<(u32, String)> {
        if generation == self.answered || generation != lock(&self.shared).generation {
            return Vec::new();
        }
        self.refusals(outcome)
    }

    fn refusals(&mut self, outcome: Outcome) -> Vec<(u32, String)> {
        match outcome {
            Ok(missing) => missing
                .into_iter()
                .filter_map(|(id, why)| self.live.remove(&id).map(|h| (h, format!("{id}: {why}"))))
                .collect(),
            Err(why) => {
                if why == DECLINED {
                    self.declined.extend(self.live.keys().cloned());
                }
                self.live
                    .drain()
                    .map(|(id, h)| (h, format!("{id}: {why}")))
                    .collect()
            }
        }
    }
}

/// Close a portal session; its shortcuts stop firing with it.
fn close(conn: &Connection, session: &OwnedObjectPath) {
    if let Ok(p) = dbus::proxy(conn, PORTAL, session.as_str(), SESSION) {
        let _ = p.call::<_, _, ()>("Close", &());
    }
}

static TOKENS: AtomicU64 = AtomicU64::new(1);

fn token() -> String {
    format!(
        "cortisoff_{}_{}",
        std::process::id(),
        TOKENS.fetch_add(1, Ordering::Relaxed)
    )
}

/// The request a portal call answers on. Its path is known before the call
/// (the sender's name and the token), so the match rule is in place before
/// the answer can be sent.
struct Request {
    responses: zbus::blocking::proxy::SignalIterator<'static>,
    token: String,
}

impl Request {
    fn new(conn: &Connection) -> Result<Self, String> {
        let sender = conn
            .unique_name()
            .ok_or("the session bus gave this connection no name")?
            .trim_start_matches(':')
            .replace('.', "_");
        let token = token();
        let path = format!("{DESKTOP}/request/{sender}/{token}");
        let proxy = dbus::proxy(conn, PORTAL, &path, REQUEST)?;
        let responses = proxy
            .receive_signal("Response")
            .map_err(|e| format!("could not wait for the portal's answer: {e}"))?;
        Ok(Self { responses, token })
    }

    /// Block until the portal answers. Zero is success, one is the user
    /// cancelling, two anything else.
    fn answer(mut self) -> Result<HashMap<String, OwnedValue>, String> {
        let message = self
            .responses
            .next()
            .ok_or("the session bus closed before the portal answered")?;
        let (code, results) = message
            .body()
            .deserialize::<(u32, HashMap<String, OwnedValue>)>()
            .map_err(|e| format!("the portal's answer did not parse: {e}"))?;
        match code {
            0 => Ok(results),
            1 => Err(DECLINED.to_owned()),
            _ => Err("the desktop portal refused the shortcuts".to_owned()),
        }
    }
}

/// Open a session and bind `shortcuts` (chord id, portal trigger) on it.
/// Answers the session and the chords the desktop did not give a key.
fn bind_all(
    conn: &Connection,
    shortcuts: &[(String, String)],
) -> Result<(OwnedObjectPath, Vec<(String, String)>), String> {
    let portal = dbus::proxy(conn, PORTAL, DESKTOP, SHORTCUTS)?;
    let fail = |e: zbus::Error| format!("the GlobalShortcuts portal failed: {e}");

    let request = Request::new(conn)?;
    let session_token = token();
    let options = HashMap::from([
        ("handle_token", Value::from(request.token.as_str())),
        ("session_handle_token", Value::from(session_token.as_str())),
    ]);
    portal
        .call::<_, _, OwnedObjectPath>("CreateSession", &(options,))
        .map_err(fail)?;
    let created = request.answer()?;
    let session = created
        .get("session_handle")
        .and_then(|v| {
            // Version 1 of the interface sends a string, later ones a path.
            String::try_from(v.clone())
                .ok()
                .or_else(|| OwnedObjectPath::try_from(v.clone()).ok().map(|p| p.to_string()))
        })
        .and_then(|s| OwnedObjectPath::try_from(s).ok())
        .ok_or("the portal opened a session but did not say which")?;

    let request = Request::new(conn)?;
    let list: Vec<(&str, HashMap<&str, Value>)> = shortcuts
        .iter()
        .map(|(id, trigger)| {
            (
                id.as_str(),
                HashMap::from([
                    ("description", Value::from(format!("cortisoff: {id}"))),
                    ("preferred_trigger", Value::from(trigger.as_str())),
                ]),
            )
        })
        .collect();
    let options = HashMap::from([("handle_token", Value::from(request.token.as_str()))]);
    let path = ObjectPath::try_from(session.as_str()).map_err(|e| e.to_string())?;
    let bound = portal
        .call::<_, _, OwnedObjectPath>("BindShortcuts", &(path, list, "", options))
        .map_err(|e| {
            close(conn, &session);
            fail(e)
        });
    bound?;
    let results = match request.answer() {
        Ok(r) => r,
        Err(e) => {
            close(conn, &session);
            return Err(e);
        }
    };
    let given: HashMap<String, String> = results
        .get("shortcuts")
        .cloned()
        .and_then(|v| Vec::<(String, HashMap<String, OwnedValue>)>::try_from(v).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(id, props)| {
            let keys = props
                .get("trigger_description")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default();
            (id, keys)
        })
        .collect();
    let missing = shortcuts
        .iter()
        .filter_map(|(id, _)| match given.get(id) {
            None => Some((
                id.clone(),
                "the shortcut dialog left this one out".to_owned(),
            )),
            Some(keys) if keys.is_empty() => Some((
                id.clone(),
                "the desktop gave this shortcut no key, which usually means another one already has it".to_owned(),
            )),
            Some(_) => None,
        })
        .collect();
    Ok((session, missing))
}

/// A chord in the shortcuts specification's text form: `CTRL+ALT+k`, with
/// the key as its XKB keysym name. `None` for a key the form has no name for.
pub fn trigger(chord: &Chord) -> Option<String> {
    let key = match chord.key {
        Key::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase().to_string(),
        Key::Char(_) => return None,
        Key::F(n) if (1..=24).contains(&n) => format!("F{n}"),
        Key::F(_) => return None,
        Key::Named(named) => match named {
            Named::Space => "space",
            Named::Enter => "Return",
            Named::Tab => "Tab",
            Named::Escape => "Escape",
            Named::Backspace => "BackSpace",
            Named::Delete => "Delete",
            Named::Insert => "Insert",
            Named::Home => "Home",
            Named::End => "End",
            Named::PageUp => "Page_Up",
            Named::PageDown => "Page_Down",
            Named::Up => "Up",
            Named::Down => "Down",
            Named::Left => "Left",
            Named::Right => "Right",
            Named::PrintScreen => "Print",
            Named::Pause => "Pause",
            Named::VolumeUp => "XF86AudioRaiseVolume",
            Named::VolumeDown => "XF86AudioLowerVolume",
            Named::VolumeMute => "XF86AudioMute",
            Named::MediaNext => "XF86AudioNext",
            Named::MediaPrevious => "XF86AudioPrev",
            Named::MediaPlayPause => "XF86AudioPlay",
            Named::MediaStop => "XF86AudioStop",
        }
        .to_owned(),
    };
    let m = chord.mods;
    let mut out = String::new();
    for (on, name) in [
        (m.ctrl, "CTRL"),
        (m.alt, "ALT"),
        (m.shift, "SHIFT"),
        (m.meta, "LOGO"),
    ] {
        if on {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&key);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Option<String> {
        trigger(&text.parse::<Chord>().unwrap())
    }

    #[test]
    fn chords_read_as_the_specification_writes_them() {
        assert_eq!(t("ctrl+alt+k").as_deref(), Some("CTRL+ALT+k"));
        assert_eq!(t("win+shift+f5").as_deref(), Some("SHIFT+LOGO+F5"));
        assert_eq!(t("alt+win+up").as_deref(), Some("ALT+LOGO+Up"));
        assert_eq!(t("ctrl+page_down").as_deref(), Some("CTRL+Page_Down"));
        assert_eq!(t("volume_mute").as_deref(), Some("XF86AudioMute"));
    }
}
