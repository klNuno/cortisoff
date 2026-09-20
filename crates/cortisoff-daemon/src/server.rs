//! Answering clients, without owning a socket.
//!
//! The transport is per platform (a named pipe on Windows, a unix socket
//! elsewhere) and it is the only part of talking to a client that needs an
//! operating system. Everything else is here: which frame gets which answer,
//! who is subscribed, and what a client is allowed to do. A platform is left
//! with accepting connections and moving bytes.

use std::collections::{BTreeMap, BTreeSet};

use cortisoff_host::{CapabilityId, CapabilitySet, Event, Host, JournalStore};
use cortisoff_ipc::{Frame, Request, RequestId, Response};

use crate::supervisor::Supervisor;

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
}

impl<J: JournalStore> Server<J> {
    pub fn new(supervisor: Supervisor<J>) -> Self {
        Self {
            supervisor,
            subscribers: BTreeSet::new(),
            previews: BTreeMap::new(),
            stopping: false,
            log: Vec::new(),
        }
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
    /// `cortisoffd` in a terminal and a user running `cortisoff trace` are
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
        probed: &CapabilitySet,
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

        let response = self.answer(client, request, host, probed);
        let mut out = vec![self.reply(client, id, response)];
        out.extend(self.fan_out());
        out
    }

    /// One turn of the daemon's loop: wait on the platform, feed whatever
    /// arrived to the rules, and hand back what subscribers should see.
    ///
    /// The platform's binary calls this and writes the result to its
    /// transport. That is the whole loop, which is the point: everything
    /// deciding anything is above this line and is tested without an OS.
    pub fn pump(&mut self, host: &mut dyn Host, timeout: std::time::Duration) -> Vec<Outbound> {
        for event in host.poll(timeout) {
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

    fn answer(
        &mut self,
        client: ClientId,
        request: Request,
        host: &mut dyn Host,
        probed: &CapabilitySet,
    ) -> Response {
        match request {
            Request::Doctor => Response::Doctor(Box::new(self.supervisor.doctor(&*host, probed))),
            Request::State => Response::State(Box::new(self.supervisor.state())),
            Request::Subscribe => {
                self.subscribers.insert(client);
                Response::Ok
            }
            Request::Unsubscribe => {
                self.subscribers.remove(&client);
                Response::Ok
            }
            Request::Set { cap, value } => match self.supervisor.manual_set(host, &cap, value) {
                Ok(()) => Response::Ok,
                Err(message) => Response::error(message),
            },
            Request::Clear { cap } => match self.supervisor.manual_clear(host, &cap) {
                Ok(()) => Response::Ok,
                Err(message) => Response::error(message),
            },
            Request::Preview { cap, value } => match self.supervisor.preview(host, &cap, value) {
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
                match self.supervisor.preview_end(host, &cap) {
                    Ok(()) => Response::Ok,
                    Err(message) => Response::error(message),
                }
            }
            Request::Rules => Response::Rules {
                rules: self.supervisor.authored().to_vec(),
            },
            Request::SaveRules { rules } => match self.supervisor.save_rules(host, rules) {
                Ok(()) => Response::Ok,
                Err(message) => Response::error(message),
            },
            Request::Reload => match self.supervisor.reload(host) {
                Ok(()) => Response::Ok,
                // A rule file with a typo in it leaves the running rules
                // exactly as they were. Losing your working configuration
                // because of a missing bracket is not acceptable.
                Err(message) => Response::error(message),
            },
            Request::Shutdown => {
                // Baselines go back here, not in the platform's exit path,
                // because the platform's exit path is the one that does not
                // run when something goes wrong.
                self.supervisor.handle(host, Event::Shutdown);
                self.stopping = true;
                self.supervisor.note_closing();
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
    use crate::startup::Interference;
    use cortisoff_core::{parse_rules, CapTable, Engine};
    use cortisoff_host::mock::{MemJournal, MockHost, Trace};
    use cortisoff_host::{ids, CapabilityId, Value};
    use cortisoff_ipc::Notification;

    const RULES: &str = r#"
[[rule]]
name = "vibrance in game"
when.app = "game.exe"
set.display.vibrance = 100
"#;

    fn setup() -> (Server<MemJournal>, MockHost) {
        let host = MockHost::new().with_actuator(ids::DISPLAY_VIBRANCE, Value::Int(50), false);
        let engine = Engine::new(
            CapTable::from_set(&host.probe()),
            parse_rules(RULES).unwrap(),
        );
        let supervisor = Supervisor::new(
            engine,
            MemJournal::new(Trace::default()),
            Interference::clear(),
        );
        (Server::new(supervisor), host)
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
        let probed = host.probe();
        let out = server.on_frame(ClientId(1), request(7, Request::State), &mut host, &probed);
        assert!(matches!(
            out[0].frame,
            Frame::Response {
                id: RequestId(7),
                ..
            }
        ));
    }

    #[test]
    fn a_manual_override_beats_the_rule_underneath_and_gives_it_back_on_clear() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);

        server.supervisor_mut().start(&mut host);
        server.supervisor_mut().handle(
            &mut host,
            Event::AppFocus(cortisoff_host::AppRef {
                exe: "game.exe".to_owned(),
                title: None,
                pid: 1,
                started_at_ms: 0,
            }),
        );
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));

        let set = Request::Set {
            cap: cap.clone(),
            value: Value::Int(10),
        };
        let out = server.on_frame(ClientId(1), request(1, set), &mut host, &probed);
        assert_eq!(responses(&out), vec![&Response::Ok]);
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(10)));

        // The rule never stopped holding, so it takes the capability back
        // rather than the screen falling all the way to its baseline.
        let clear = Request::Clear { cap };
        server.on_frame(ClientId(1), request(2, clear), &mut host, &probed);
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));
    }

    #[test]
    fn setting_a_capability_this_machine_lacks_is_a_sentence_not_a_panic() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        let set = Request::Set {
            cap: CapabilityId::new("display.color_temp"),
            value: Value::Int(3400),
        };
        let out = server.on_frame(ClientId(1), request(1, set), &mut host, &probed);
        assert!(matches!(responses(&out)[0], Response::Error { .. }));
    }

    #[test]
    fn only_subscribers_are_sent_notifications() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        server.on_frame(
            ClientId(1),
            request(1, Request::Subscribe),
            &mut host,
            &probed,
        );
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

        server.on_frame(
            ClientId(1),
            request(2, Request::Unsubscribe),
            &mut host,
            &probed,
        );
        server.supervisor_mut().handle(
            &mut host,
            Event::Idle {
                idle: false,
                seconds: 0,
            },
        );
        assert!(server.fan_out().is_empty());
    }

    /// A closed terminal is how `cortisoff trace` normally ends.
    #[test]
    fn a_disconnected_subscriber_stops_being_written_to() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        server.on_frame(
            ClientId(1),
            request(1, Request::Subscribe),
            &mut host,
            &probed,
        );
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
    fn shutting_down_restores_the_screen_before_the_loop_stops() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        server.supervisor_mut().start(&mut host);
        server.supervisor_mut().handle(
            &mut host,
            Event::AppFocus(cortisoff_host::AppRef {
                exe: "game.exe".to_owned(),
                title: None,
                pid: 1,
                started_at_ms: 0,
            }),
        );

        server.on_frame(
            ClientId(1),
            request(1, Request::Shutdown),
            &mut host,
            &probed,
        );
        assert!(server.stopping());
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(50)));
    }

    #[test]
    fn a_subscriber_is_told_the_daemon_is_closing() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        server.on_frame(
            ClientId(1),
            request(1, Request::Subscribe),
            &mut host,
            &probed,
        );
        let out = server.on_frame(
            ClientId(1),
            request(2, Request::Shutdown),
            &mut host,
            &probed,
        );
        assert!(out.iter().any(|o| matches!(
            &o.frame,
            Frame::Notification {
                notification: Notification::Closing
            }
        )));
    }

    /// The daemon's whole loop, driven the way a platform drives it.
    #[test]
    fn pumping_feeds_the_platform_queue_to_the_rules_and_the_subscribers() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        server.supervisor_mut().start(&mut host);
        server.on_frame(
            ClientId(1),
            request(1, Request::Subscribe),
            &mut host,
            &probed,
        );

        host.queue(Event::AppFocus(cortisoff_host::AppRef {
            exe: "game.exe".to_owned(),
            title: None,
            pid: 1,
            started_at_ms: 0,
        }));
        let out = server.pump(&mut host, std::time::Duration::from_millis(0));

        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));
        assert!(out.iter().any(|o| matches!(
            &o.frame,
            Frame::Notification {
                notification: Notification::RuleEdge { holding: true, .. }
            }
        )));
    }

    /// A quiet second is the common case and it has to cost nothing.
    #[test]
    fn pumping_with_nothing_to_report_produces_nothing() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        server.supervisor_mut().start(&mut host);
        server.on_frame(
            ClientId(1),
            request(1, Request::Subscribe),
            &mut host,
            &probed,
        );
        server.fan_out();
        assert!(server
            .pump(&mut host, std::time::Duration::from_millis(0))
            .is_empty());
    }

    /// A rule deleted from the file must not leave its value on the screen
    /// with nothing owning it any more.
    #[test]
    fn reloading_drops_the_claims_of_a_rule_that_is_gone() {
        let dir = std::env::temp_dir().join("cortisoff-reload-ok");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rules.toml");
        std::fs::write(&file, RULES).unwrap();

        let host = MockHost::new().with_actuator(ids::DISPLAY_VIBRANCE, Value::Int(50), false);
        let probed = host.probe();
        let engine = Engine::new(CapTable::from_set(&probed), parse_rules(RULES).unwrap());
        let supervisor = Supervisor::new(
            engine,
            MemJournal::new(Trace::default()),
            Interference::clear(),
        )
        .with_rules_path(&file);
        let mut server = Server::new(supervisor);
        let mut host = host;

        server.supervisor_mut().start(&mut host);
        server.supervisor_mut().handle(
            &mut host,
            Event::AppFocus(cortisoff_host::AppRef {
                exe: "game.exe".to_owned(),
                title: None,
                pid: 1,
                started_at_ms: 0,
            }),
        );
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));

        std::fs::write(&file, "").unwrap();
        let out = server.on_frame(ClientId(1), request(1, Request::Reload), &mut host, &probed);
        assert_eq!(responses(&out), vec![&Response::Ok]);
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(50)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Losing a working configuration to a missing bracket is not acceptable,
    /// so a file that does not parse changes nothing.
    #[test]
    fn a_rule_file_that_does_not_parse_leaves_the_running_rules_alone() {
        let dir = std::env::temp_dir().join("cortisoff-reload-broken");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rules.toml");
        std::fs::write(&file, RULES).unwrap();

        let host = MockHost::new().with_actuator(ids::DISPLAY_VIBRANCE, Value::Int(50), false);
        let probed = host.probe();
        let engine = Engine::new(CapTable::from_set(&probed), parse_rules(RULES).unwrap());
        let supervisor = Supervisor::new(
            engine,
            MemJournal::new(Trace::default()),
            Interference::clear(),
        )
        .with_rules_path(&file);
        let mut server = Server::new(supervisor);
        let mut host = host;

        server.supervisor_mut().start(&mut host);
        server.supervisor_mut().handle(
            &mut host,
            Event::AppFocus(cortisoff_host::AppRef {
                exe: "game.exe".to_owned(),
                title: None,
                pid: 1,
                started_at_ms: 0,
            }),
        );

        std::fs::write(&file, "[[rule]]\nname = \"broken\"\nwhen.nonsense = true\n").unwrap();
        let out = server.on_frame(ClientId(1), request(1, Request::Reload), &mut host, &probed);
        assert!(matches!(responses(&out)[0], Response::Error { .. }));
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_client_sending_a_response_is_told_rather_than_dropped() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        let frame = Frame::Response {
            id: RequestId(1),
            response: Response::Ok,
        };
        let out = server.on_frame(ClientId(1), frame, &mut host, &probed);
        assert!(matches!(responses(&out)[0], Response::Error { .. }));
    }

    /// A dashboard that dies mid-drag must not leave its preview on the
    /// screen: the disconnect ends it, and the fade back starts.
    #[test]
    fn a_client_that_disconnects_mid_drag_has_its_preview_ended() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);
        server.on_frame(
            ClientId(1),
            request(
                1,
                Request::Preview {
                    cap: cap.clone(),
                    value: Value::Int(90),
                },
            ),
            &mut host,
            &probed,
        );
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(90)));

        server.disconnect(ClientId(1), &mut host);
        assert!(
            host.timers
                .iter()
                .any(|(id, ..)| *id == cortisoff_core::PREVIEW_TIMER),
            "the fade back asked for its first step"
        );
    }

    /// The whole thing through the wire: drag, release, and a mock actuator
    /// that ends up where it started once every step has run.
    #[test]
    fn a_preview_fades_back_to_the_actuator_rest_over_the_wire() {
        let (mut server, mut host) = setup();
        let probed = host.probe();
        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);
        server.on_frame(
            ClientId(1),
            request(
                1,
                Request::Preview {
                    cap: cap.clone(),
                    value: Value::Int(100),
                },
            ),
            &mut host,
            &probed,
        );
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));
        let out = server.on_frame(
            ClientId(1),
            request(2, Request::PreviewEnd { cap: cap.clone() }),
            &mut host,
            &probed,
        );
        assert_eq!(responses(&out)[0], &Response::Ok);

        // The rest was read before the drag, when the driver still showed 50.
        let mut values = Vec::new();
        for _ in 0..40 {
            server.supervisor_mut().handle(
                &mut host,
                cortisoff_host::Event::TimerFired {
                    id: cortisoff_core::PREVIEW_TIMER,
                },
            );
            values.push(host.value(ids::DISPLAY_VIBRANCE).unwrap());
        }
        assert_eq!(values.last(), Some(&Value::Int(50)));
        assert!(
            values.iter().filter(|v| **v != Value::Int(50)).count() >= 10,
            "{values:?}"
        );
        assert_eq!(
            host.cancelled,
            vec![cortisoff_core::PREVIEW_TIMER],
            "the last step told the host to stop the fade timer, once"
        );
    }
}
