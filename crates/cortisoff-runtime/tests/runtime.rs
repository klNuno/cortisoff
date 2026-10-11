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
    let cs2 = Event::AppFocus(cortisoff_host::AppRef {
        exe: "CS2.exe".into(),
        title: None,
        pid: 1,
        started_at_ms: 0,
    });
    rig.world.absorb(&cs2);
    rig.with(|env| rt.on_event(env, &cs2));
    for r in rt.reports() {
        assert!(r.error.is_none(), "{}: {:?}", r.id, r.error);
    }
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
