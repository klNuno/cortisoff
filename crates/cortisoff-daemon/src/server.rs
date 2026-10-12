//! Answering clients, without owning a socket.
//!
//! The transport is per platform (a named pipe on Windows, a unix socket
//! elsewhere) and it is the only part of talking to a client that needs an
//! operating system. Everything else is here: which frame gets which answer,
//! who is subscribed, and what a client is allowed to do. A platform is left
//! with accepting connections and moving bytes.

use std::collections::{BTreeMap, BTreeSet};

use cortisoff_host::{CapabilityId, Host, JournalStore};
use cortisoff_ipc::{Frame, Notification, Request, RequestId, Response};

use crate::supervisor::Supervisor;
use crate::updates::{Found, Staged, Updates};

/// One connected client, assigned by the transport. Reused ids are the
/// transport's problem: a disconnect drops every trace of the old one first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientId(pub u64);

/// A frame to write, and who to write it to.
#[derive(Debug, Clone, PartialEq)]
pub struct Outbound {
    pub to: ClientId,
    pub frame: Frame,
}

/// A notification is fanned out once and consumed. The daemon's own log is
/// not a client and has no socket, so without this it would see nothing the
/// moment a real subscriber attached, and everything otherwise.
const MAX_LOG: usize = 4096;

pub struct Server<J> {
    supervisor: Supervisor<J>,
    subscribers: BTreeSet<ClientId>,
    /// Which client is dragging which slider, so a dashboard that dies
    /// mid-drag has its preview ended for it on disconnect.
    previews: BTreeMap<ClientId, BTreeSet<CapabilityId>>,
    stopping: bool,
    log: Vec<cortisoff_ipc::Notification>,
    updates: Updates,
}

impl<J: JournalStore> Server<J> {
    pub fn new(supervisor: Supervisor<J>) -> Self {
        let updates = Updates::new(supervisor.auto_update());
        Self {
            supervisor,
            subscribers: BTreeSet::new(),
            previews: BTreeMap::new(),
            stopping: false,
            log: Vec::new(),
            updates,
        }
    }

    pub fn updates_mut(&mut self) -> &mut Updates {
        &mut self.updates
    }

    /// What the update thread found, told to every subscriber.
    pub fn found(&mut self, found: Found) {
        self.updates.found(found);
        self.announce_updates();
    }

    /// A staged build, once nothing is applied to any screen. The loop asks
    /// after every turn, so a build that waits is looked at again whenever
    /// claims change or anything else wakes the loop; nothing polls for it.
    pub fn take_update(&mut self) -> Option<Staged> {
        if !self.updates.waiting() {
            return None;
        }
        let quiet = self.supervisor.quiet();
        self.updates.take(quiet)
    }

    /// The staged build could not be put in place. This one keeps running.
    pub fn update_failed(&mut self, message: String) {
        self.updates.failed(message);
        self.announce_updates();
    }

    fn announce_updates(&mut self) {
        let report = Box::new(self.updates.report());
        self.supervisor.notify(Notification::Updates(report));
    }

    pub fn supervisor(&self) -> &Supervisor<J> {
        &self.supervisor
    }

    pub fn supervisor_mut(&mut self) -> &mut Supervisor<J> {
        &mut self.supervisor
    }

    /// True once a client has asked the daemon to exit. The platform stops its
    /// loop on this, after the shutdown event has already put every baseline
    /// back.
    pub fn stopping(&self) -> bool {
        self.stopping
    }

    /// Everything that has happened since the last call, for the daemon's own
    /// log. Independent of the subscriber list on purpose: a user watching
    /// `cortisoff --daemon` in a terminal and a user running `cortisoff-cli trace` are
    /// both entitled to the whole stream.
    pub fn drain_log(&mut self) -> Vec<cortisoff_ipc::Notification> {
        std::mem::take(&mut self.log)
    }

    /// A client went away. Never an error: a subscriber closing its terminal
    /// is the normal end of `cortisoff trace`. A preview it was holding ends
    /// as if the drag had, and whatever that does reaches the subscribers.
    pub fn disconnect(&mut self, client: ClientId, host: &mut dyn Host) -> Vec<Outbound> {
        self.subscribers.remove(&client);
        for cap in self.previews.remove(&client).unwrap_or_default() {
            let _ = self.supervisor.preview_end(host, &cap);
        }
        self.fan_out()
    }

    /// Answer one frame, and fan out whatever the daemon did about it.
    ///
    /// The whole exchange is one call so ordering is not up to the platform:
    /// the answer to a request is written before the notifications the request
    /// caused.
    pub fn on_frame(
        &mut self,
        client: ClientId,
        frame: Frame,
        host: &mut dyn Host,
    ) -> Vec<Outbound> {
        let (id, request) = match frame {
            Frame::Request { id, request } => (id, request),
            // Only the daemon sends these. A client that does is confused
            // rather than hostile, and it gets told rather than dropped.
            Frame::Response { id, .. } => {
                return vec![self.reply(
                    client,
                    id,
                    Response::error("a client does not send responses"),
                )]
            }
            Frame::Notification { .. } => {
                return vec![self.reply(
                    client,
                    RequestId(0),
                    Response::error("a client does not send notifications"),
                )]
            }
        };

        let response = self.answer(client, request, host);
        let mut out = vec![self.reply(client, id, response)];
        out.extend(self.fan_out());
        out
    }

    /// One turn of the daemon's loop: wait on the platform, feed whatever
    /// arrived to the plugins, and hand back what subscribers should see.
    ///
    /// The platform's binary calls this and writes the result to its
    /// transport. That is the whole loop, which is the point: everything
    /// deciding anything is above this line and is tested without an OS.
    pub fn pump(&mut self, host: &mut dyn Host, timeout: std::time::Duration) -> Vec<Outbound> {
        let events = host.poll(timeout);
        if events.is_empty() {
            // Woken with nothing to say: an HTTP answer a plugin waits for.
            self.supervisor.idle_turn(host);
        }
        for event in events {
            self.supervisor.handle(host, event);
        }
        self.fan_out()
    }

    /// Everything the supervisor has produced since the last call, addressed
    /// to every subscriber. The platform calls this after each event it pushes
    /// as well, which is what makes `cortisoff trace` live.
    pub fn fan_out(&mut self) -> Vec<Outbound> {
        let pending = self.supervisor.drain();
        if pending.len() + self.log.len() <= MAX_LOG {
            self.log.extend(pending.iter().cloned());
        }
        if self.subscribers.is_empty() || pending.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(pending.len() * self.subscribers.len());
        for notification in pending {
            for &to in &self.subscribers {
                out.push(Outbound {
                    to,
                    frame: Frame::Notification {
                        notification: notification.clone(),
                    },
                });
            }
        }
        out
    }

    fn answer(&mut self, client: ClientId, request: Request, host: &mut dyn Host) -> Response {
        let s = &mut self.supervisor;
        let done = |r: Result<(), String>| match r {
            Ok(()) => Response::Ok,
            Err(message) => Response::error(message),
        };
        match request {
            Request::Doctor => Response::Doctor(Box::new(s.doctor(&*host))),
            Request::State => Response::State(Box::new(s.state())),
            Request::Subscribe => {
                self.subscribers.insert(client);
                Response::Ok
            }
            Request::Unsubscribe => {
                self.subscribers.remove(&client);
                Response::Ok
            }
            Request::Set { cap, value } => done(s.manual_set(host, &cap, value)),
            Request::Clear { cap } => done(s.manual_clear(host, &cap)),
            Request::Preview { cap, value } => match s.preview(host, &cap, value) {
                Ok(()) => {
                    self.previews.entry(client).or_default().insert(cap);
                    Response::Ok
                }
                Err(message) => Response::error(message),
            },
            Request::PreviewEnd { cap } => {
                if let Some(caps) = self.previews.get_mut(&client) {
                    caps.remove(&cap);
                }
                done(s.preview_end(host, &cap))
            }
            Request::Plugins => Response::Plugins {
                plugins: s.plugins(),
            },
            Request::Enable { id, enabled } => done(s.set_enabled(host, &id, enabled)),
            Request::Settings { id, settings } => done(s.set_settings(host, &id, settings)),
            Request::Message { id, name, data } => done(s.message(host, &id, &name, &data)),
            Request::Rescan => done(s.rescan(host)),
            Request::Link { path } => done(s.link(host, &path).map(|_| ())),
            Request::Remove { id } => done(s.remove(host, &id)),
            Request::Windows => Response::Windows {
                windows: host.windows(),
            },
            Request::Updates { auto } => {
                if let Some(on) = auto {
                    s.set_auto_update(on);
                    self.updates.set_auto(on);
                    let report = Box::new(self.updates.report());
                    s.notify(Notification::Updates(report));
                }
                Response::Updates(Box::new(self.updates.report()))
            }
            Request::Shutdown => {
                // Baselines go back here, not in the platform's exit path,
                // because the platform's exit path is the one that does not
                // run when something goes wrong.
                s.shutdown(host);
                s.note_closing();
                self.stopping = true;
                Response::Ok
            }
        }
    }

    fn reply(&self, to: ClientId, id: RequestId, response: Response) -> Outbound {
        Outbound {
            to,
            frame: Frame::Response { id, response },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::testing::{plugin, supervisor};
    use cortisoff_core::FADE_TIMER;
    use cortisoff_host::mock::{MemJournal, MockHost};
    use cortisoff_host::{ids, AppRef, CapabilityId, Event, Value};

    const GAME: &str = "return {
        start = function(cx) cx:watch('app.focus') end,
        on = function(cx, s, ev)
          if ev.kind ~= 'app.focus' then return end
          if ev.exe == 'game.exe' then cx:claim('display.vibrance', 100, { priority = 100 })
          else cx:release('display.vibrance') end
        end,
    }";

    fn setup() -> (Server<MemJournal>, MockHost) {
        let host = MockHost::new().with_actuator(ids::DISPLAY_VIBRANCE, Value::Int(50), false);
        let sup = supervisor(&host, vec![plugin("game", GAME)]);
        (Server::new(sup), host)
    }

    fn focus(exe: &str) -> Event {
        Event::AppFocus(AppRef {
            exe: exe.to_owned(),
            title: None,
            pid: 1,
            started_at_ms: 0,
        })
    }

    fn request(id: u64, request: Request) -> Frame {
        Frame::Request {
            id: RequestId(id),
            request,
        }
    }

    fn responses(out: &[Outbound]) -> Vec<&Response> {
        out.iter()
            .filter_map(|o| match &o.frame {
                Frame::Response { response, .. } => Some(response),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_response_carries_the_id_of_the_request_it_answers() {
        let (mut server, mut host) = setup();
        let out = server.on_frame(ClientId(1), request(7, Request::State), &mut host);
        assert!(matches!(
            out[0].frame,
            Frame::Response {
                id: RequestId(7),
                ..
            }
        ));
    }

    #[test]
    fn a_manual_override_beats_the_plugin_and_gives_it_back_on_clear() {
        let (mut server, mut host) = setup();
        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);
        server.supervisor_mut().start(&mut host);
        server.supervisor_mut().handle(&mut host, focus("game.exe"));
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));

        let set = Request::Set {
            cap: cap.clone(),
            value: Value::Int(10),
        };
        let out = server.on_frame(ClientId(1), request(1, set), &mut host);
        assert_eq!(responses(&out), vec![&Response::Ok]);
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(10)));

        server.on_frame(ClientId(1), request(2, Request::Clear { cap }), &mut host);
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));
    }

    #[test]
    fn setting_a_capability_this_machine_lacks_is_a_sentence_not_a_panic() {
        let (mut server, mut host) = setup();
        let set = Request::Set {
            cap: CapabilityId::new(ids::DISPLAY_COLOR_TEMP),
            value: Value::Int(3400),
        };
        let out = server.on_frame(ClientId(1), request(1, set), &mut host);
        assert!(matches!(responses(&out)[0], Response::Error { .. }));
    }

    #[test]
    fn plugins_are_listed_and_switched_over_the_wire() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        let out = server.on_frame(ClientId(1), request(1, Request::Plugins), &mut host);
        let Response::Plugins { plugins } = responses(&out)[0] else {
            panic!("{out:?}");
        };
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].running);

        let off = Request::Enable {
            id: "game".into(),
            enabled: false,
        };
        let out = server.on_frame(ClientId(1), request(2, off), &mut host);
        assert_eq!(responses(&out), vec![&Response::Ok]);
        assert!(!server.supervisor().plugins()[0].running);

        let wrong = Request::Enable {
            id: "nope".into(),
            enabled: true,
        };
        let out = server.on_frame(ClientId(1), request(3, wrong), &mut host);
        assert!(matches!(responses(&out)[0], Response::Error { .. }));
    }

    #[test]
    fn only_subscribers_are_sent_notifications() {
        let (mut server, mut host) = setup();
        server.on_frame(ClientId(1), request(1, Request::Subscribe), &mut host);
        server.supervisor_mut().drain();

        server.supervisor_mut().handle(
            &mut host,
            Event::Idle {
                idle: true,
                seconds: 300,
            },
        );
        let out = server.fan_out();
        assert!(!out.is_empty());
        assert!(out.iter().all(|o| o.to == ClientId(1)));

        server.on_frame(ClientId(1), request(2, Request::Unsubscribe), &mut host);
        server.supervisor_mut().handle(
            &mut host,
            Event::Idle {
                idle: false,
                seconds: 0,
            },
        );
        assert!(server.fan_out().is_empty());
    }

    /// A closed terminal is how `cortisoff-cli trace` normally ends.
    #[test]
    fn a_disconnected_subscriber_stops_being_written_to() {
        let (mut server, mut host) = setup();
        server.on_frame(ClientId(1), request(1, Request::Subscribe), &mut host);
        server.disconnect(ClientId(1), &mut host);
        server.supervisor_mut().handle(
            &mut host,
            Event::Idle {
                idle: true,
                seconds: 300,
            },
        );
        assert!(server.fan_out().is_empty());
    }

    #[test]
    fn shutting_down_restores_the_screen_and_says_so() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        server.supervisor_mut().handle(&mut host, focus("game.exe"));
        server.on_frame(ClientId(1), request(1, Request::Subscribe), &mut host);

        let out = server.on_frame(ClientId(1), request(2, Request::Shutdown), &mut host);
        assert!(server.stopping());
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(50)));
        assert!(out.iter().any(|o| matches!(
            &o.frame,
            Frame::Notification {
                notification: Notification::Closing
            }
        )));
    }

    /// The daemon's whole loop, driven the way a platform drives it.
    #[test]
    fn pumping_feeds_the_platform_queue_to_the_plugins_and_the_subscribers() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        server.on_frame(ClientId(1), request(1, Request::Subscribe), &mut host);
        server.fan_out();

        host.queue(focus("game.exe"));
        let out = server.pump(&mut host, std::time::Duration::from_millis(0));
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));
        assert!(out.iter().any(|o| matches!(
            &o.frame,
            Frame::Notification {
                notification: Notification::Action(_)
            }
        )));
    }

    /// A quiet second is the common case and it has to cost nothing.
    #[test]
    fn pumping_with_nothing_to_report_produces_nothing() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        server.on_frame(ClientId(1), request(1, Request::Subscribe), &mut host);
        server.fan_out();
        assert!(server
            .pump(&mut host, std::time::Duration::from_millis(0))
            .is_empty());
    }

    #[test]
    fn a_client_sending_a_response_is_told_rather_than_dropped() {
        let (mut server, mut host) = setup();
        let frame = Frame::Response {
            id: RequestId(1),
            response: Response::Ok,
        };
        let out = server.on_frame(ClientId(1), frame, &mut host);
        assert!(matches!(responses(&out)[0], Response::Error { .. }));
    }

    fn staged(version: &str) -> Found {
        Found::Staged {
            path: std::env::temp_dir().join(format!("cortisoff-server-test-{version}")),
            version: version.to_owned(),
        }
    }

    /// An update staged while a plugin holds a capability waits, and goes the
    /// first turn after the claim is let go and has faded out.
    #[test]
    fn a_staged_update_waits_until_nothing_is_held() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        server.supervisor_mut().handle(&mut host, focus("game.exe"));
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));

        server.found(staged("9.0.0"));
        assert_eq!(server.take_update(), None, "applied while a claim is held");

        host.queue(focus("notepad.exe"));
        server.pump(&mut host, std::time::Duration::from_millis(0));
        for _ in 0..100 {
            if server.supervisor().quiet() {
                break;
            }
            assert_eq!(server.take_update(), None, "applied mid-fade");
            host.now_ms += 40;
            server
                .supervisor_mut()
                .handle(&mut host, Event::TimerFired { id: FADE_TIMER });
        }
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(50)));
        let update = server.take_update().expect("applied once nothing is held");
        assert_eq!(update.version, "9.0.0");
        assert_eq!(server.take_update(), None, "handed over once");
    }

    #[test]
    fn a_staged_update_is_not_applied_with_automatic_installs_off() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        server.found(staged("9.0.0"));
        let off = Request::Updates { auto: Some(false) };
        server.on_frame(ClientId(1), request(1, off), &mut host);
        assert_eq!(server.take_update(), None);
    }

    /// The setting goes through the wire both ways, is stored, and every
    /// subscriber hears it changed.
    #[test]
    fn the_update_setting_round_trips_through_the_protocol() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        server.on_frame(ClientId(2), request(1, Request::Subscribe), &mut host);
        server.fan_out();

        let read = Request::Updates { auto: None };
        let out = server.on_frame(ClientId(1), request(2, read.clone()), &mut host);
        let Response::Updates(report) = responses(&out)[0] else {
            panic!("{out:?}");
        };
        assert!(report.auto, "on by default");
        assert!(
            !out.iter().any(|o| o.to == ClientId(2)),
            "a read says nothing"
        );

        let off = Request::Updates { auto: Some(false) };
        let out = server.on_frame(ClientId(1), request(3, off), &mut host);
        let Response::Updates(report) = responses(&out)[0] else {
            panic!("{out:?}");
        };
        assert!(!report.auto);
        assert!(!server.supervisor().auto_update(), "stored");
        assert!(out.iter().any(|o| o.to == ClientId(2)
            && matches!(
                &o.frame,
                Frame::Notification {
                    notification: Notification::Updates(r)
                } if !r.auto
            )));

        let out = server.on_frame(ClientId(1), request(4, read), &mut host);
        assert!(matches!(responses(&out)[0], Response::Updates(r) if !r.auto));
    }

    /// A window that dies mid-drag must not leave its preview on the screen:
    /// the disconnect ends it and the fade back runs to the end.
    #[test]
    fn a_client_that_disconnects_mid_drag_has_its_preview_ended() {
        let (mut server, mut host) = setup();
        server.supervisor_mut().start(&mut host);
        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);
        let preview = Request::Preview {
            cap,
            value: Value::Int(90),
        };
        server.on_frame(ClientId(1), request(1, preview), &mut host);
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(90)));

        server.disconnect(ClientId(1), &mut host);
        let mut values = Vec::new();
        for _ in 0..40 {
            host.now_ms += 40;
            server
                .supervisor_mut()
                .handle(&mut host, Event::TimerFired { id: FADE_TIMER });
            values.push(host.value(ids::DISPLAY_VIBRANCE).unwrap());
        }
        assert_eq!(values.last(), Some(&Value::Int(50)));
        assert!(
            values.iter().filter(|v| **v != Value::Int(50)).count() >= 5,
            "{values:?}"
        );
    }
}
