//! The runtime against a mock host: what plugins ask for comes back as
//! commands, and what the host says reaches the plugins that listen.

use std::collections::HashMap;

use cortisoff_core::World;
use cortisoff_host::mock::MockHost;
use cortisoff_host::{CapabilityId, Event, Host, Value as HostValue};
use cortisoff_ipc::Origin;
use cortisoff_runtime::{Command, Env, Files, Manifest, Package, Runtime, Store};

fn plugin(id: &str, script: &str) -> Package {
    let manifest = format!(
        "id = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\nplatforms = [\"windows\", \"linux\", \"macos\"]\n"
    );
    let files: &'static [(&'static str, &'static str)] = Box::leak(Box::new([
        (
            "plugin.toml",
            &*Box::leak(manifest.clone().into_boxed_str()),
        ),
        ("init.lua", &*Box::leak(script.to_owned().into_boxed_str())),
    ]));
    Package {
        manifest: Manifest::parse(&manifest).unwrap(),
        files: Files::Builtin(files),
        origin: Origin::Installed,
        url: None,
    }
}

fn on(ids: &[&str]) -> Store {
    let mut store = Store::in_memory();
    for id in ids {
        store.plugins.insert(
            (*id).to_owned(),
            cortisoff_runtime::store::Entry {
                enabled: true,
                settings: serde_json::Value::Null,
            },
        );
    }
    store
}

struct Rig {
    host: MockHost,
    world: World,
    /// What the plugins hold, as the claim stack would answer `cx:holds`:
    /// each capability with the monotonic time its claim ends, `u64::MAX`
    /// while claimed. Kept from the commands [`Rig::run`] sees.
    held: HashMap<String, u64>,
}

impl Rig {
    fn new() -> Self {
        let mut host = MockHost::new()
            .with_actuator("display.color_temp", HostValue::Int(6500), false)
            .with_actuator("display.vibrance", HostValue::Int(50), false)
            .with_actuator("display.mode", HostValue::Text("x".into()), false)
            .with_actuator("wallpaper.playback", HostValue::Bool(true), false);
        host.clock = Some(Event::TimeOfDay {
            minute: 22 * 60,
            second: 0,
            millisecond: 0,
        });
        host.now_ms = 1_000;
        Rig {
            host,
            world: World::default(),
            held: HashMap::new(),
        }
    }

    /// A colour temperature that holds one monitor apart, as on Windows.
    fn apart() -> Self {
        let mut rig = Self::new();
        rig.host = std::mem::take(&mut rig.host).with_outputs("display.color_temp");
        rig
    }

    fn with<R>(&mut self, f: impl FnOnce(&mut Env) -> R) -> R {
        let probed = self.host.probe();
        let now = self.host.monotonic_ms();
        let held = &self.held;
        let holds =
            |_: u32, cap: &CapabilityId| held.get(cap.as_str()).is_some_and(|&end| now < end);
        let mut env = Env {
            host: &mut self.host,
            world: &self.world,
            probed: &probed,
            holds: &holds,
            now,
        };
        f(&mut env)
    }

    /// A call whose commands the rig keeps track of, as the claim stack
    /// would: a release fades out over its length, unless one already under
    /// way ends sooner.
    fn run(&mut self, f: impl FnOnce(&mut Env) -> Vec<Command>) -> Vec<Command> {
        let out = self.with(f);
        self.track(&out);
        out
    }

    fn track(&mut self, out: &[Command]) {
        let now = self.host.monotonic_ms();
        for c in out {
            match c {
                Command::Claim { cap, .. } => {
                    self.held.insert(cap.to_string(), u64::MAX);
                }
                Command::Release { cap, fade_ms, .. } => {
                    if let Some(end) = self.held.get_mut(cap.as_str()) {
                        *end = (*end).min(now + fade_ms);
                    }
                }
                Command::ReleaseAll { .. } => self.held.clear(),
                _ => {}
            }
        }
    }
}

fn claims(out: &[Command]) -> Vec<(String, HostValue)> {
    out.iter()
        .filter_map(|c| match c {
            Command::Claim { cap, value, .. } => Some((cap.to_string(), value.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_claim_on_start_becomes_a_command_and_stopping_releases_it() {
    let p = plugin(
        "warm",
        "return { start = function(cx, s) assert(cx:claim('display.color_temp', 3200.0, { fade = 500 })) end }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["warm"]), None);
    let out = rig.with(|env| rt.start_all(env));
    assert_eq!(
        claims(&out),
        vec![("display.color_temp".to_owned(), HostValue::Int(3200))]
    );
    let key = rt.key_of("warm").unwrap();
    assert!(out
        .iter()
        .any(|c| matches!(c, Command::Claim { owner, fade_ms: 500, .. } if *owner == key)));

    let out = rig.with(|env| rt.set_enabled(env, "warm", false)).unwrap();
    assert!(out
        .iter()
        .any(|c| matches!(c, Command::ReleaseAll { owner, .. } if *owner == key)));
    assert!(out.contains(&Command::Save));
    assert!(!rt.active());
}

#[test]
fn a_capability_the_machine_lacks_is_refused_with_a_reason() {
    let p = plugin(
        "x",
        "return { start = function(cx, s) local ok, err = cx:claim('display.hue', 1) cx:publish({ ok = ok, err = err }) end }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["x"]), None);
    rig.with(|env| rt.start_all(env));
    let report = rt.report("x").unwrap();
    assert_eq!(report.state["ok"], serde_json::Value::Null);
    assert!(report.state["err"]
        .as_str()
        .unwrap()
        .contains("display.hue"));
}

#[test]
fn timers_fire_when_due_and_repeat() {
    let p = plugin(
        "tick",
        "local n = 0
         return { start = function(cx, s)
           cx:every(1000, function(cx, s) n = n + 1 cx:publish({ n = n }) end)
         end }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["tick"]), None);
    rig.with(|env| rt.start_all(env));
    assert_eq!(rt.next_due(), Some(2_000));

    rig.host.now_ms = 2_000;
    rig.with(|env| rt.on_timer(env));
    assert_eq!(rt.report("tick").unwrap().state["n"].as_f64(), Some(1.0));
    assert_eq!(rt.next_due(), Some(3_000));

    rig.host.now_ms = 2_500;
    rig.with(|env| rt.on_timer(env));
    assert_eq!(rt.report("tick").unwrap().state["n"].as_f64(), Some(1.0));
}

#[test]
fn a_hotkey_types_text() {
    let p = plugin(
        "keys",
        "return { start = function(cx, s)
           assert(cx:hotkey('ctrl+alt+t', function(cx, s) cx:type('hi ' .. cx:clipboard()) end))
         end }",
    );
    let mut rig = Rig::new();
    rig.host.clipboard_text = "there".into();
    let mut rt = Runtime::new(vec![p], &on(&["keys"]), None);
    rig.with(|env| rt.start_all(env));
    let wanted = rt.take_hotkeys().unwrap();
    assert_eq!(wanted.len(), 1);
    assert_eq!(wanted[0].chord.to_string(), "ctrl+alt+t");
    assert!(rt.take_hotkeys().is_none(), "handed over once");

    rig.with(|env| rt.on_hotkey(env, wanted[0].id));
    assert_eq!(rig.host.desktop, vec!["type hi there".to_owned()]);

    rig.with(|env| rt.set_enabled(env, "keys", false)).unwrap();
    assert_eq!(
        rt.take_hotkeys().unwrap().len(),
        0,
        "stopping drops its hotkeys"
    );
}

#[test]
fn watched_sources_reach_the_host_and_events_reach_the_plugin() {
    let p = plugin(
        "idle",
        "return {
           start = function(cx, s) cx:watch('user.idle', { after = 300 }) cx:watch('app.focus') end,
           on = function(cx, s, ev) cx:publish({ kind = ev.kind, exe = ev.exe, idle = ev.idle }) end,
         }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["idle"]), None);
    rig.with(|env| rt.start_all(env));
    let (caps, thresholds) = rt.take_sources().unwrap();
    assert!(caps.contains(&CapabilityId::new("user.idle")));
    assert!(caps.contains(&CapabilityId::new("app.focus")));
    assert_eq!(thresholds, vec![300]);

    rig.with(|env| {
        rt.on_event(
            env,
            &Event::Idle {
                idle: true,
                seconds: 300,
            },
        )
    });
    assert_eq!(
        rt.report("idle").unwrap().state["idle"],
        serde_json::json!(true)
    );

    rig.with(|env| {
        rt.on_event(
            env,
            &Event::AppFocus(cortisoff_host::AppRef {
                exe: "cs2.exe".into(),
                title: None,
                pid: 1,
                started_at_ms: 0,
            }),
        )
    });
    assert_eq!(rt.report("idle").unwrap().state["exe"], "cs2.exe");
}

/// A watch with its own threshold hears `idle = true` when that threshold is
/// crossed, even under the host's five-minute default, and `idle = false`
/// when the user comes back; another watch's threshold says nothing to it.
#[test]
fn an_idle_watch_hears_its_own_threshold() {
    let p = plugin(
        "idle",
        "return {
           start = function(cx, s) cx:watch('user.idle', { after = 120 }) end,
           on = function(cx, s, ev) cx:publish({ idle = ev.idle, seconds = ev.seconds }) end,
         }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["idle"]), None);
    rig.with(|env| rt.start_all(env));
    let mut hear = |idle: bool, seconds: u32| {
        rig.with(|env| rt.on_event(env, &Event::Idle { idle, seconds }));
        let state = rt.report("idle").unwrap().state;
        (state["idle"].clone(), state["seconds"].clone())
    };
    use serde_json::json;
    // The host's flag is still false at two minutes: its default is five.
    assert_eq!(hear(false, 120), (json!(true), json!(120)));
    // Another plugin's threshold: unchanged.
    assert_eq!(hear(true, 300), (json!(true), json!(120)));
    assert_eq!(hear(false, 0), (json!(false), json!(0)));
}

/// Hosts report the full path of the application in front, and plugins
/// match on its file name, so `exe` is the name and `path` the whole path,
/// in an event and in `cx:world()` alike.
#[test]
fn a_plugin_sees_the_file_name_of_the_application_in_front() {
    let p = plugin(
        "focus",
        "return {
           start = function(cx, s) cx:watch('app.focus') end,
           on = function(cx, s, ev)
             local app = cx:world().app
             cx:publish({ exe = ev.exe, path = ev.path, world = app and app.exe })
           end,
         }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["focus"]), None);
    rig.with(|env| rt.start_all(env));
    let path = if cfg!(windows) {
        r"C:\Games\Counter-Strike\CS2.exe"
    } else {
        "/opt/games/cs2.exe"
    };
    let focus = Event::AppFocus(cortisoff_host::AppRef {
        exe: path.into(),
        title: None,
        pid: 1,
        started_at_ms: 0,
    });
    rig.world.absorb(&focus);
    rig.with(|env| rt.on_event(env, &focus));
    let state = &rt.report("focus").unwrap().state;
    assert_eq!(state["exe"], "cs2.exe");
    assert_eq!(state["path"], path);
    assert_eq!(state["world"], "cs2.exe");
}

#[test]
fn settings_from_the_window_are_stored_as_a_difference_and_heard() {
    let p = plugin(
        "s",
        "return {
           settings = { night = 3200, fade = 30 },
           on = function(cx, s, ev) cx:publish({ kind = ev.kind, night = s.night }) end,
         }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["s"]), None);
    rig.with(|env| rt.start_all(env));
    assert_eq!(
        rt.report("s").unwrap().settings["night"].as_f64(),
        Some(3200.0)
    );

    let out = rig
        .with(|env| rt.set_settings(env, "s", serde_json::json!({ "night": 2700, "fade": 30 })))
        .unwrap();
    assert!(out.contains(&Command::Save));
    let report = rt.report("s").unwrap();
    assert_eq!(report.state["kind"], "settings");
    assert_eq!(report.state["night"].as_f64(), Some(2700.0));

    let mut store = Store::in_memory();
    rt.store_into(&mut store);
    assert_eq!(
        store.entry("s").settings,
        serde_json::json!({ "night": 2700 })
    );
}

#[test]
fn a_plugin_that_fails_fails_alone() {
    let bad = plugin(
        "bad",
        "return { start = function(cx, s) error('boom') end }",
    );
    let good = plugin(
        "good",
        "return { start = function(cx, s) cx:claim('display.vibrance', 80) end }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![bad, good], &on(&["bad", "good"]), None);
    let out = rig.with(|env| rt.start_all(env));
    assert!(rt.report("bad").unwrap().error.unwrap().contains("boom"));
    assert_eq!(claims(&out).len(), 1);
    assert!(rt.report("good").unwrap().error.is_none());
}

#[test]
fn messages_from_the_page_reach_on() {
    let p = plugin(
        "m",
        "return { on = function(cx, s, ev) if ev.kind == 'message' then cx:publish({ got = ev.name, minutes = ev.data.minutes }) end end }",
    );
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["m"]), None);
    rig.with(|env| rt.start_all(env));
    rig.with(|env| rt.message(env, "m", "pause", &serde_json::json!({ "minutes": 60 })))
        .unwrap();
    let st = rt.report("m").unwrap().state;
    assert_eq!(st["got"], "pause");
    assert_eq!(st["minutes"].as_f64(), Some(60.0));
}

#[test]
fn a_plugin_for_another_system_does_not_start() {
    let manifest = "id = \"w\"\nname = \"W\"\nversion = \"1\"\nplatforms = [\"macos\"]\n";
    let mut p = plugin(
        "w",
        "return { start = function(cx) cx:claim('display.vibrance', 1) end }",
    );
    p.manifest = Manifest::parse(manifest).unwrap();
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &on(&["w"]), None).with_os("windows");
    let out = rig.with(|env| rt.start_all(env));
    assert!(claims(&out).is_empty());
    let r = rt.report("w").unwrap();
    assert!(!r.supported);
    assert!(r.error.unwrap().contains("macos"));
}

#[test]
fn enabling_a_plugin_for_another_system_is_refused_with_its_reason() {
    let manifest = "id = \"w\"
name = \"W\"
version = \"1\"
platforms = [\"windows\"]
";
    let mut p = plugin("w", "return {}");
    p.manifest = Manifest::parse(manifest).unwrap();
    let mut rig = Rig::new();
    let mut rt = Runtime::new(vec![p], &Store::in_memory(), None).with_os("macos");
    let refused = rig.with(|env| rt.set_enabled(env, "w", true)).unwrap_err();
    assert_eq!(refused, "made for windows, not macos");
    assert!(!rt.report("w").unwrap().enabled);
    // Already on from an older build: still refused, and off still works.
    let mut rt = Runtime::new(
        vec![{
            let mut p = plugin("w", "return {}");
            p.manifest = Manifest::parse(manifest).unwrap();
            p
        }],
        &on(&["w"]),
        None,
    )
    .with_os("macos");
    assert!(rig.with(|env| rt.set_enabled(env, "w", true)).is_err());
    rig.with(|env| rt.set_enabled(env, "w", false)).unwrap();
    assert!(!rt.report("w").unwrap().enabled);
}

fn night_light() -> Vec<Package> {
    cortisoff_runtime::builtin()
        .into_iter()
        .filter(|p| p.manifest.id == "night-light")
        .collect()
}

fn night_light_with(settings: serde_json::Value) -> Store {
    let mut store = on(&["night-light"]);
    store.plugins.get_mut("night-light").unwrap().settings = settings;
    store
}

/// The fade asked of cortisoff with the night colour, in milliseconds.
fn night_fade(out: &[Command]) -> Option<u64> {
    out.iter().find_map(|c| match c {
        Command::Claim { cap, fade_ms, .. } if cap.as_str() == "display.color_temp" => {
            Some(*fade_ms)
        }
        _ => None,
    })
}

/// Night light wakes at the edges of the night and nowhere else: cortisoff
/// runs the fade, so nothing steps it from Lua.
#[test]
fn night_light_sleeps_until_the_next_edge() {
    let due_in = |minute: u16| {
        let mut rig = Rig::new();
        rig.host.clock = Some(Event::TimeOfDay {
            minute,
            second: 0,
            millisecond: 0,
        });
        let mut rt = Runtime::new(night_light(), &on(&["night-light"]), None).with_os("windows");
        rig.with(|env| rt.start_all(env));
        assert!(rt.report("night-light").unwrap().error.is_none());
        rt.next_due().unwrap() - rig.host.now_ms
    };
    // The defaults: warm from 20:00 to 07:00.
    assert_eq!(due_in(12 * 60), 8 * 3_600_000 + 50, "noon waits for 20:00");
    assert_eq!(
        due_in(20 * 60 + 10),
        650 * 60_000 + 50,
        "the evening waits for 07:00"
    );
    assert_eq!(
        due_in(6 * 60 + 40),
        20 * 60_000 + 50,
        "so does the end of the night"
    );
}

/// The fade is in seconds and zero is at once. A stored `fade` from before,
/// in minutes, is read as minutes and capped at five.
#[test]
fn night_light_fades_in_seconds_and_zero_is_instant() {
    let fade_for = |settings: serde_json::Value| {
        let mut rig = Rig::new();
        let mut rt =
            Runtime::new(night_light(), &night_light_with(settings), None).with_os("windows");
        let out = rig.with(|env| rt.start_all(env));
        assert!(rt.report("night-light").unwrap().error.is_none());
        night_fade(&out)
    };
    assert_eq!(fade_for(serde_json::json!({ "fade_seconds": 0 })), Some(0));
    assert_eq!(
        fade_for(serde_json::json!({ "fade_seconds": 45 })),
        Some(45_000)
    );
    assert_eq!(fade_for(serde_json::json!({})), Some(60_000), "the default");
    assert_eq!(fade_for(serde_json::json!({ "fade": 2 })), Some(120_000));
    assert_eq!(fade_for(serde_json::json!({ "fade": 30 })), Some(300_000));
}

fn fullscreen(exclusive: bool, outputs: &[&str]) -> Event {
    Event::Fullscreen {
        exclusive,
        outputs: outputs.iter().map(|o| o.to_string()).collect(),
    }
}

/// A fullscreen game with no screen named sends the colour away everywhere,
/// and its end brings it back, both in a quick fade whatever the evening one
/// is. A maximised window changes nothing: a browser maximised all evening
/// is not a film.
#[test]
fn night_light_stands_aside_for_fullscreen_only_and_comes_back_quickly() {
    let mut rig = Rig::new();
    let store = night_light_with(serde_json::json!({ "fade_seconds": 300 }));
    let mut rt = Runtime::new(night_light(), &store, None).with_os("windows");
    let out = rig.with(|env| rt.start_all(env));
    assert_eq!(night_fade(&out), Some(300_000));

    let maximized = Event::Maximized { maximized: true };
    rig.world.absorb(&maximized);
    let out = rig.with(|env| rt.on_event(env, &maximized));
    assert!(
        !out.iter().any(|c| matches!(c, Command::Release { .. })),
        "{out:?}"
    );
    assert_eq!(rt.report("night-light").unwrap().state["holding"], true);

    let full = fullscreen(true, &[]);
    rig.world.absorb(&full);
    let out = rig.with(|env| rt.on_event(env, &full));
    assert!(
        out.iter()
            .any(|c| matches!(c, Command::Release { cap, fade_ms: 800, .. } if cap.as_str() == "display.color_temp")),
        "{out:?}"
    );
    assert_eq!(rt.report("night-light").unwrap().state["reason"], "filled");

    let back = fullscreen(false, &[]);
    rig.world.absorb(&back);
    let out = rig.with(|env| rt.on_event(env, &back));
    assert_eq!(night_fade(&out), Some(800));
}

/// Even set to instant, a fullscreen game fades the colour away: a snap from
/// 3200 K to daylight as a game opens reads as a glitch.
#[test]
fn night_light_fades_out_of_the_way_even_when_set_to_instant() {
    let mut rig = Rig::new();
    let store = night_light_with(serde_json::json!({ "fade_seconds": 0 }));
    let mut rt = Runtime::new(night_light(), &store, None).with_os("windows");
    rig.with(|env| rt.start_all(env));
    let full = fullscreen(true, &[]);
    rig.world.absorb(&full);
    let out = rig.with(|env| rt.on_event(env, &full));
    assert!(
        out.iter()
            .any(|c| matches!(c, Command::Release { fade_ms: 800, .. })),
        "{out:?}"
    );
}

/// A game fullscreen on one monitor takes the warmth off that monitor only.
/// It stays off while the focus is on another monitor, and comes back when
/// the game leaves.
#[test]
fn night_light_spares_only_the_screen_a_game_fills() {
    let mut rig = Rig::apart();
    let store = night_light_with(serde_json::json!({ "fade_seconds": 300 }));
    let mut rt = Runtime::new(night_light(), &store, None).with_os("windows");
    rig.with(|env| rt.start_all(env));
    let one = r"display.color_temp@\\.\DISPLAY3";

    let full = fullscreen(true, &[r"\\.\DISPLAY3"]);
    rig.world.absorb(&full);
    let out = rig.with(|env| rt.on_event(env, &full));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Claim { cap, value: HostValue::Int(6500), fade_ms: 800, .. } if cap.as_str() == one)),
        "{out:?}"
    );
    assert!(
        !out.iter().any(|c| matches!(c, Command::Release { .. })),
        "the other screens stay warm: {out:?}"
    );
    let state = rt.report("night-light").unwrap().state;
    assert_eq!(state["holding"], true);
    assert_eq!(state["spared"], 1);

    // The focus goes to another monitor, the game stays on its own.
    let away = fullscreen(false, &[r"\\.\DISPLAY3"]);
    rig.world.absorb(&away);
    let out = rig.with(|env| rt.on_event(env, &away));
    assert!(
        !out.iter().any(|c| matches!(c, Command::Release { .. })),
        "{out:?}"
    );

    let gone = fullscreen(false, &[]);
    rig.world.absorb(&gone);
    let out = rig.with(|env| rt.on_event(env, &gone));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Release { cap, fade_ms: 800, .. } if cap.as_str() == one)),
        "{out:?}"
    );
    assert_eq!(rt.report("night-light").unwrap().state["spared"], 0);
}

/// `minute` of the day on the wall clock, with the monotonic clock moved in
/// step. Later in the test means later in the day.
fn at(rig: &mut Rig, minute: u16) -> Event {
    let clock = Event::TimeOfDay {
        minute,
        second: 0,
        millisecond: 0,
    };
    rig.host.clock = Some(clock.clone());
    rig.host.now_ms = minute as u64 * 60_000;
    clock
}

/// The morning lets the night go over its fade while a game still fills a
/// screen: that screen stays spared, or it rejoins the whole and shows the
/// night again for the length of the fade. A game opened while the morning
/// still fades is spared too, and one opened after it is left alone.
#[test]
fn night_light_keeps_the_game_spared_through_the_morning() {
    let mut rig = Rig::apart();
    let store = night_light_with(serde_json::json!({ "fade_seconds": 300 }));
    let mut rt = Runtime::new(night_light(), &store, None).with_os("windows");
    rig.run(|env| rt.start_all(env));
    let one = r"display.color_temp@\\.\DISPLAY3";
    let two = r"display.color_temp@\\.\DISPLAY1";
    let full = fullscreen(true, &[r"\\.\DISPLAY3"]);
    rig.world.absorb(&full);
    rig.run(|env| rt.on_event(env, &full));

    let morning = at(&mut rig, 7 * 60);
    let out = rig.run(|env| rt.on_event(env, &morning));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Release { cap, fade_ms: 300_000, .. } if cap.as_str() == "display.color_temp")),
        "{out:?}"
    );
    assert!(
        !out.iter()
            .any(|c| matches!(c, Command::Release { cap, .. } if cap.as_str() == one)),
        "the game's screen stays spared: {out:?}"
    );
    assert_eq!(rt.report("night-light").unwrap().state["spared"], 1);

    // A second game on another screen, two minutes into the morning fade.
    let both = fullscreen(true, &[r"\\.\DISPLAY3", r"\\.\DISPLAY1"]);
    rig.world.absorb(&both);
    at(&mut rig, 7 * 60 + 2);
    let out = rig.run(|env| rt.on_event(env, &both));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Claim { cap, value: HostValue::Int(6500), fade_ms: 800, .. } if cap.as_str() == two)),
        "{out:?}"
    );

    // The games leave once the morning is over: both screens go back.
    let gone = fullscreen(false, &[]);
    rig.world.absorb(&gone);
    at(&mut rig, 8 * 60);
    let out = rig.run(|env| rt.on_event(env, &gone));
    for cap in [one, two] {
        assert!(
            out.iter().any(|c| matches!(c,
                Command::Release { cap: c, fade_ms: 800, .. } if c.as_str() == cap)),
            "{out:?}"
        );
    }

    // In the middle of the day a game is left alone: nothing to spare it from.
    rig.world.absorb(&full);
    let out = rig.run(|env| rt.on_event(env, &full));
    assert!(
        !out.iter()
            .any(|c| matches!(c, Command::Claim { cap, .. } if cap.as_str() == one)),
        "{out:?}"
    );
}

/// The PC slept through the morning edge: the night lets go when it wakes,
/// so a game opened a minute later still has a warm whole to be spared from,
/// whatever the schedule says. Once that fade is over, a game is left alone.
#[test]
fn night_light_spares_a_game_during_a_morning_fade_that_started_late() {
    let mut rig = Rig::apart();
    let store = night_light_with(serde_json::json!({ "fade_seconds": 300 }));
    let mut rt = Runtime::new(night_light(), &store, None).with_os("windows");
    rig.run(|env| rt.start_all(env));
    let one = r"display.color_temp@\\.\DISPLAY3";
    let two = r"display.color_temp@\\.\DISPLAY1";

    let wake = at(&mut rig, 8 * 60);
    let out = rig.run(|env| rt.on_event(env, &wake));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Release { cap, fade_ms: 300_000, .. } if cap.as_str() == "display.color_temp")),
        "{out:?}"
    );

    let full = fullscreen(true, &[r"\\.\DISPLAY3"]);
    rig.world.absorb(&full);
    at(&mut rig, 8 * 60 + 1);
    let out = rig.run(|env| rt.on_event(env, &full));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Claim { cap, value: HostValue::Int(6500), fade_ms: 800, .. } if cap.as_str() == one)),
        "{out:?}"
    );

    let other = fullscreen(true, &[r"\\.\DISPLAY3", r"\\.\DISPLAY1"]);
    rig.world.absorb(&other);
    at(&mut rig, 8 * 60 + 10);
    let out = rig.run(|env| rt.on_event(env, &other));
    assert!(
        !out.iter()
            .any(|c| matches!(c, Command::Claim { cap, .. } if cap.as_str() == two)),
        "{out:?}"
    );
}

/// The hardware held the morning fade up, so it ends later than the plugin
/// asked: a game opened after the asked length, while the others are still
/// warm, is spared all the same. Once the fade is over, it is not.
#[test]
fn night_light_spares_a_game_while_a_held_up_morning_fade_goes_on() {
    let mut rig = Rig::apart();
    let store = night_light_with(serde_json::json!({ "fade_seconds": 10 }));
    let mut rt = Runtime::new(night_light(), &store, None).with_os("windows");
    rig.run(|env| rt.start_all(env));
    let one = r"display.color_temp@\\.\DISPLAY3";
    let two = r"display.color_temp@\\.\DISPLAY1";

    let wake = at(&mut rig, 8 * 60);
    let out = rig.run(|env| rt.on_event(env, &wake));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Release { cap, fade_ms: 10_000, .. } if cap.as_str() == "display.color_temp")),
        "{out:?}"
    );
    // Ten seconds of a batch stuck in the hardware: the fade waits as long.
    *rig.held.get_mut("display.color_temp").unwrap() += 10_000;

    let full = fullscreen(true, &[r"\\.\DISPLAY3"]);
    rig.world.absorb(&full);
    rig.host.now_ms += 11_050;
    let out = rig.run(|env| rt.on_event(env, &full));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Claim { cap, value: HostValue::Int(6500), fade_ms: 800, .. } if cap.as_str() == one)),
        "{out:?}"
    );

    let other = fullscreen(true, &[r"\\.\DISPLAY3", r"\\.\DISPLAY1"]);
    rig.world.absorb(&other);
    rig.host.now_ms += 9_000;
    let out = rig.run(|env| rt.on_event(env, &other));
    assert!(
        !out.iter()
            .any(|c| matches!(c, Command::Claim { cap, .. } if cap.as_str() == two)),
        "{out:?}"
    );
}

/// A pause while a game fills a screen lets the others go and keeps the
/// game's screen where it is, instead of letting it go to a whole that is
/// still warm.
#[test]
fn night_light_pauses_around_the_game_without_warming_it() {
    let mut rig = Rig::apart();
    let mut rt = Runtime::new(night_light(), &on(&["night-light"]), None).with_os("windows");
    rig.run(|env| rt.start_all(env));
    let one = r"display.color_temp@\\.\DISPLAY3";
    let full = fullscreen(true, &[r"\\.\DISPLAY3"]);
    rig.world.absorb(&full);
    rig.run(|env| rt.on_event(env, &full));
    let out = rig
        .with(|env| {
            rt.message(
                env,
                "night-light",
                "pause",
                &serde_json::json!({ "minutes": 10 }),
            )
        })
        .unwrap();
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Release { cap, fade_ms: 800, .. } if cap.as_str() == "display.color_temp")),
        "{out:?}"
    );
    assert!(
        !out.iter()
            .any(|c| matches!(c, Command::Release { cap, .. } if cap.as_str() == one)),
        "{out:?}"
    );
}

/// Where one monitor cannot be held apart, a fullscreen game pauses every
/// screen, as it always did.
#[test]
fn night_light_pauses_every_screen_where_one_cannot_be_spared() {
    let mut rig = Rig::new();
    let mut rt = Runtime::new(night_light(), &on(&["night-light"]), None).with_os("windows");
    rig.with(|env| rt.start_all(env));
    let full = fullscreen(true, &["A"]);
    rig.world.absorb(&full);
    let out = rig.with(|env| rt.on_event(env, &full));
    assert!(
        out.iter().any(|c| matches!(c,
            Command::Release { cap, fade_ms: 800, .. } if cap.as_str() == "display.color_temp")),
        "{out:?}"
    );
    assert_eq!(rt.report("night-light").unwrap().state["reason"], "filled");
}

/// Display profiles says so when the session never names the app in front,
/// as on GNOME Wayland, instead of waiting for an app that never comes.
#[test]
fn display_profiles_says_when_no_app_is_ever_in_front() {
    let status_of = |rig: &mut Rig| {
        let packages: Vec<Package> = cortisoff_runtime::builtin()
            .into_iter()
            .filter(|p| p.manifest.id == "display-profiles")
            .collect();
        let mut rt = Runtime::new(packages, &on(&["display-profiles"]), None).with_os("linux");
        rig.with(|env| rt.start_all(env));
        let report = rt.report("display-profiles").unwrap();
        assert!(report.error.is_none(), "{:?}", report.error);
        report.state["can_focus"].clone()
    };
    let mut without = Rig::new();
    assert_eq!(status_of(&mut without), serde_json::json!(false));
    let mut with = Rig::new();
    with.host = std::mem::take(&mut with.host).with_source("app.focus");
    assert_eq!(status_of(&mut with), serde_json::json!(true));
}

/// Every plugin shipped inside cortisoff starts, reacts and stops against a
/// host offering everything, without a Lua error.
#[test]
fn the_builtin_plugins_run() {
    let ids: Vec<String> = cortisoff_runtime::builtin()
        .iter()
        .map(|p| p.manifest.id.clone())
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut rig = Rig::new();
    rig.world.maximized = false;
    let mut rt = Runtime::new(cortisoff_runtime::builtin(), &on(&refs), None).with_os("windows");
    let out = rig.with(|env| rt.start_all(env));
    for r in rt.reports() {
        assert!(r.running, "{} is not running: {:?}", r.id, r.error);
        assert!(r.error.is_none(), "{}: {:?}", r.id, r.error);
    }
    // 22:00 is inside the default night: the night light holds a warm value.
    assert!(
        claims(&out)
            .iter()
            .any(|(cap, v)| cap == "display.color_temp"
                && matches!(v, HostValue::Int(k) if *k < 6500)),
        "{out:?}"
    );
    assert!(
        !rt.take_hotkeys().unwrap().is_empty(),
        "shortcuts registered its defaults"
    );

    rig.world.absorb(&Event::Maximized { maximized: true });
    rig.with(|env| rt.on_event(env, &Event::Maximized { maximized: true }));
    rig.with(|env| {
        rt.message(
            env,
            "night-light",
            "pause",
            &serde_json::json!({ "minutes": 1 }),
        )
    })
    .unwrap();
    rig.host.now_ms += 31_000;
    rig.with(|env| rt.on_timer(env));
    // The daemon folds every event into the world before plugins hear it.
    // A full path, as every host reports it.
    let cs2 = Event::AppFocus(cortisoff_host::AppRef {
        exe: r"C:\Program Files (x86)\Steam\steamapps\common\cs2\game\bin\win64\cs2.exe".into(),
        title: None,
        pid: 1,
        started_at_ms: 0,
    });
    rig.world.absorb(&cs2);
    rig.with(|env| rt.on_event(env, &cs2));
    for r in rt.reports() {
        assert!(r.error.is_none(), "{}: {:?}", r.id, r.error);
    }
    let locked = Event::SessionLock { locked: true };
    rig.world.absorb(&locked);
    rig.with(|env| rt.on_event(env, &locked));
    assert_eq!(
        rt.report("wallpaper-pause").unwrap().state["paused"],
        serde_json::json!(true)
    );
    assert_eq!(
        rt.report("display-profiles").unwrap().state["profile"],
        "cs2.exe"
    );
    let out = rig.with(|env| rt.shutdown(env));
    assert!(out.iter().any(|c| matches!(c, Command::ReleaseAll { .. })));
    for r in rt.reports() {
        assert!(r.error.is_none(), "{}: {:?}", r.id, r.error);
    }
}
