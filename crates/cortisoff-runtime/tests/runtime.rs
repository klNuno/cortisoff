//! The runtime against a mock host: what plugins ask for comes back as
//! commands, and what the host says reaches the plugins that listen.

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
        }
    }

    fn with<R>(&mut self, f: impl FnOnce(&mut Env) -> R) -> R {
        let probed = self.host.probe();
        let now = self.host.monotonic_ms();
        let mut env = Env {
            host: &mut self.host,
            world: &self.world,
            probed: &probed,
            now,
        };
        f(&mut env)
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

/// A fullscreen game sends the colour away and its end brings it back within
/// a second, whatever the evening fade. A maximised window changes nothing:
/// a browser maximised all evening is not a film.
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

    let full = Event::Fullscreen { exclusive: true };
    rig.world.absorb(&full);
    let out = rig.with(|env| rt.on_event(env, &full));
    assert!(
        out.iter()
            .any(|c| matches!(c, Command::Release { fade_ms: 1000, .. })),
        "{out:?}"
    );
    assert_eq!(rt.report("night-light").unwrap().state["reason"], "filled");

    let back = Event::Fullscreen { exclusive: false };
    rig.world.absorb(&back);
    let out = rig.with(|env| rt.on_event(env, &back));
    assert_eq!(night_fade(&out), Some(1000));
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
