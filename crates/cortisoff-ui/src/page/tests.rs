use super::*;
use egui::{pos2, Event, PointerButton, RawInput};
use serde_json::json;

const NOW: f64 = 22.0 * 60.0;

fn report_for(p: &Package) -> PluginReport {
    PluginReport {
        id: p.manifest.id.clone(),
        name: p.manifest.name.clone(),
        version: p.manifest.version.clone(),
        description: p.manifest.description.clone(),
        icon: p.manifest.icon.clone(),
        author: p.manifest.author.clone(),
        platforms: p.manifest.platforms.clone(),
        supported: true,
        origin: p.origin,
        dir: p.files.dir().map(|d| d.display().to_string()),
        url: None,
        ffi: p.manifest.ffi,
        enabled: true,
        running: true,
        error: None,
        settings: Json::Null,
        state: Json::Null,
        holds: Vec::new(),
    }
}

fn builtin_report(id: &str) -> PluginReport {
    let p = builtin().into_iter().find(|p| p.manifest.id == id).unwrap();
    report_for(&p)
}

/// A plugin in its own folder, as a linked one is.
struct Linked {
    dir: std::path::PathBuf,
}

impl Linked {
    fn new(name: &str, script: &str) -> Linked {
        let dir =
            std::env::temp_dir().join(format!("cortisoff-page-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "id = \"{name}\"\nname = \"Test\"\nversion = \"0.1.0\"\nplatforms = [\"windows\", \"linux\", \"macos\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("init.lua"), script).unwrap();
        Linked { dir }
    }

    fn report(&self) -> PluginReport {
        let p = Package::from_dir(&self.dir, Origin::Linked).unwrap();
        report_for(&p)
    }
}

impl Drop for Linked {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// Draws `page` headless for one frame per entry of `inputs`, the theme
/// installed first, and returns every effect.
fn run(page: &mut Page, inputs: Vec<Vec<Event>>) -> Vec<Effect> {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut all = Vec::new();
    for events in inputs {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(760.0, 1600.0))),
            events,
            ..RawInput::default()
        };
        let mut output = ctx.run_ui(input, |ui| all.extend(page.draw(ui, NOW)));
        output.textures_delta.clear();
    }
    all
}

fn frames(page: &mut Page, n: usize) -> Vec<Effect> {
    run(page, (0..n).map(|_| Vec::new()).collect())
}

#[test]
fn every_builtin_page_runs_without_an_error() {
    let states = [
        ("night-light", json!({ "kelvin": 3400, "holding": true })),
        (
            "display-profiles",
            json!({ "app": "cs2.exe", "profile": "cs2.exe", "held": ["vibrance 80 %"],
                    "problems": [], "can_mode": true, "can_vibrance": true }),
        ),
        (
            "wallpaper-pause",
            json!({ "paused": true, "reason": "on battery" }),
        ),
        (
            "shortcuts",
            json!({ "results": [{ "ok": true }, { "ok": false, "error": "taken" }, { "ok": true }] }),
        ),
    ];
    let ids: Vec<String> = builtin().into_iter().map(|p| p.manifest.id).collect();
    assert_eq!(
        ids.len(),
        states.len(),
        "a built-in plugin has no test: {ids:?}"
    );
    for (id, state) in states {
        let mut report = builtin_report(id);
        let mut page = Page::new(&report);
        assert_eq!(page.error(), None, "{id} did not load");

        let bare = page.status(NOW);
        assert!(
            bare.is_some(),
            "{id}: status without state: {:?}",
            page.error()
        );

        report.state = state;
        page.sync(&report);
        let status = page
            .status(NOW)
            .unwrap_or_else(|| panic!("{id}: {:?}", page.error()));
        assert!(!status.line.is_empty(), "{id}: an empty status line");

        let effects = frames(&mut page, 4);
        assert_eq!(page.error(), None, "{id}: {:?}", page.error());
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Settings(_))),
            "{id} changed its settings by being drawn: {effects:?}"
        );
    }
}

#[test]
fn night_light_reads_its_state() {
    let mut report = builtin_report("night-light");
    report.state = json!({ "reason": "paused", "paused_until": "23:10" });
    let mut page = Page::new(&report);
    let status = page.status(NOW).unwrap();
    assert_eq!(status.state, State::Paused);
    assert_eq!(status.detail, "Back at 23:10");
}

#[test]
fn an_edit_made_by_lua_comes_back_as_settings() {
    let plugin = Linked::new(
        "edit",
        "return { settings = { n = 1, list = { 'a' } },
                  page = function(ui, s) s.n = 3200.0; s.list = {} end }",
    );
    let mut page = Page::new(&plugin.report());
    let effects = frames(&mut page, 3);
    let settings: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Settings(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(settings, vec![json!({ "n": 3200, "list": [] })]);
}

#[test]
fn send_reaches_the_daemon_half() {
    let plugin = Linked::new(
        "send",
        "return { page = function(ui, s) ui:send('pause', { minutes = 60 }); ui:send('resume') end }",
    );
    let mut page = Page::new(&plugin.report());
    let effects = frames(&mut page, 1);
    assert_eq!(
        effects,
        vec![
            Effect::Send {
                name: "pause".into(),
                data: json!({ "minutes": 60 })
            },
            Effect::Send {
                name: "resume".into(),
                data: Json::Null
            },
        ]
    );
}

#[test]
fn a_lua_error_is_shown_not_raised() {
    let plugin = Linked::new(
        "boom",
        "return {
           status = function(s) return nil + 1 end,
           page = function(ui, s)
             ui:card('Before', function(ui) ui:label('drawn first') end)
             ui:card(nil, function(ui) error('the page broke') end)
           end }",
    );
    let mut page = Page::new(&plugin.report());
    assert_eq!(page.status(NOW), None);
    let status_error = page.error().unwrap().to_owned();
    assert!(status_error.contains("arithmetic"), "{status_error}");

    let effects = frames(&mut page, 2);
    assert!(effects.is_empty(), "{effects:?}");
    let error = page.error().unwrap();
    assert!(error.contains("the page broke"), "{error}");
    assert!(error.contains("init.lua"), "{error}");
    assert!(!error.contains("traceback"), "{error}");
    assert!(!error.contains('\n'), "{error}");
}

#[test]
fn a_page_that_cannot_load_says_why() {
    let plugin = Linked::new("broken", "return {");
    let mut page = Page::new(&plugin.report());
    assert!(
        page.error().unwrap().contains("init.lua"),
        "{:?}",
        page.error()
    );
    assert_eq!(page.status(NOW), None);
    assert!(frames(&mut page, 1).is_empty());
}

#[test]
fn a_plugin_without_a_page_draws_a_hint() {
    let plugin = Linked::new("quiet", "return { settings = { a = 1 } }");
    let mut page = Page::new(&plugin.report());
    assert!(frames(&mut page, 2).is_empty());
    assert_eq!(page.error(), None);
    assert_eq!(page.status(NOW), None);
}

#[test]
fn a_fresh_edit_wins_over_an_older_echo() {
    let plugin = Linked::new(
        "echo",
        "local set = false
         return { settings = { night = 3200 },
                  page = function(ui, s) if not set then s.night = 2700; set = true end end }",
    );
    let mut report = plugin.report();
    let mut page = Page::new(&report);
    frames(&mut page, 1);
    assert_eq!(page.settings, json!({ "night": 2700 }));

    // The daemon has not seen the edit yet.
    report.settings = json!({ "night": 3200 });
    page.sync(&report);
    assert_eq!(page.settings, json!({ "night": 2700 }));

    // Long after, the daemon is the truth again.
    page.emitted = Some(Instant::now() - ECHO * 2);
    page.sync(&report);
    assert_eq!(page.settings, json!({ "night": 3200 }));
}

#[test]
fn a_linked_plugin_reloads_when_its_file_changes() {
    let plugin = Linked::new(
        "reload",
        "return { status = function() return { line = 'first' } end }",
    );
    let report = plugin.report();
    let mut page = Page::new(&report);
    assert_eq!(page.status(NOW).unwrap().line, "first");
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(
        plugin.dir.join("init.lua"),
        "return { status = function() return { line = 'second' } end }",
    )
    .unwrap();
    page.checked = None;
    page.sync(&report);
    assert_eq!(page.status(NOW).unwrap().line, "second");

    // A save that does not parse keeps the version that worked.
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(plugin.dir.join("init.lua"), "return {").unwrap();
    page.checked = None;
    page.sync(&report);
    assert!(page.error().is_some());
    assert_eq!(page.status(NOW).unwrap().line, "second");
}

#[test]
fn a_preview_slider_previews_while_dragged() {
    // The slider is the first thing on the page: its rail runs from x = 8
    // to x = 212 on the row at y = 15.
    let plugin = Linked::new(
        "preview",
        "return { settings = { k = 1900 },
                  page = function(ui, s)
                    s.k = ui:slider(s.k, { min = 1900, max = 6500, step = 100, preview = 'display.color_temp' })
                  end }",
    );
    let mut page = Page::new(&plugin.report());
    let at = |x: f32| Event::PointerMoved(pos2(x, 15.0));
    let button = |x: f32, pressed: bool| Event::PointerButton {
        pos: pos2(x, 15.0),
        button: PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    let effects = run(
        &mut page,
        vec![
            vec![],
            vec![at(110.0), button(110.0, true)],
            vec![at(150.0)],
            vec![at(180.0)],
            vec![button(180.0, false)],
            vec![],
        ],
    );
    let cap = CapabilityId::new("display.color_temp");
    let previews: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Preview { cap: c, value } if *c == cap => Some(value.clone()),
            _ => None,
        })
        .collect();
    assert!(previews.len() >= 2, "{effects:?}");
    assert!(
        previews.iter().all(|v| matches!(v, Value::Int(_))),
        "{previews:?}"
    );
    let ends = effects
        .iter()
        .filter(|e| matches!(e, Effect::PreviewEnd { cap: c } if *c == cap))
        .count();
    assert_eq!(ends, 1, "{effects:?}");
    assert!(matches!(
        effects.last(),
        Some(Effect::Settings(_)) | Some(Effect::PreviewEnd { .. })
    ));
}

#[test]
fn values_compare_as_settings() {
    assert!(same(&json!({ "a": 3200 }), &json!({ "a": 3200.0 })));
    assert!(same(&json!({ "a": [] }), &json!({ "a": {} })));
    assert!(!same(&json!({ "a": [1] }), &json!({ "a": {} })));
    assert_eq!(
        tidy(json!({ "a": 3200.0, "b": 0.5 })),
        json!({ "a": 3200, "b": 0.5 })
    );
    assert_eq!(widgets::parse_clock("21:30"), Some(1290.0));
    assert_eq!(widgets::parse_clock("930"), Some(570.0));
    assert_eq!(widgets::parse_clock("7"), Some(420.0));
    assert_eq!(widgets::parse_clock("25:00"), None);
    assert_eq!(widgets::clock(1290.0), "21:30");
}

/// A PNG of each built-in page at 760 px wide, through wgpu. Needs a GPU,
/// so CI skips it. `CORTISOFF_CAPTURES` names the folder, by default
/// `target/captures/pages`.
#[test]
#[ignore = "renders with wgpu"]
fn capture_every_builtin_page() {
    let dir = std::env::var_os("CORTISOFF_CAPTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/captures/pages")
        });
    std::fs::create_dir_all(&dir).unwrap();
    let states = [
        ("night-light", json!({ "kelvin": 3400, "holding": true })),
        (
            "display-profiles",
            json!({ "app": "cs2.exe", "profile": "cs2.exe", "held": ["vibrance 80 %"],
                    "problems": [], "can_mode": true, "can_vibrance": true }),
        ),
        ("wallpaper-pause", json!({ "paused": false })),
        (
            "shortcuts",
            json!({ "results": [{ "ok": true }, { "ok": true }, { "ok": true }] }),
        ),
    ];
    for (id, state) in states {
        let mut report = builtin_report(id);
        report.state = state;
        let page = capture(Page::new(&report), &dir.join(format!("page-{id}.png")));
        assert_eq!(page.error(), None, "{id}");
    }
    // How a page that raises an error looks, for the eye only.
    let broken = Linked::new(
        "broken-page",
        "return { page = function(ui, s) ui:card('Drawn', function(ui) end); local t = nil; return t.x end }",
    );
    let page = capture(Page::new(&broken.report()), &dir.join("error-card.png"));
    assert!(page.error().is_some());
}

/// Draws `page` at 760 px wide and as tall as it is, and saves it.
fn capture(page: Page, path: &Path) -> Page {
    use egui_kittest::Harness;

    let mut harness = Harness::builder()
        .with_size([760.0, 1400.0])
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_ui_state(
            |ui, (page, installed, bottom): &mut (Page, bool, f32)| {
                // Fonts bind one frame late, and the harness runs its
                // first frame while it is built.
                if !*installed {
                    theme::install(ui.ctx());
                    *installed = true;
                    return;
                }
                Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                    let _ = page.draw(ui, NOW);
                });
                *bottom = ui.min_rect().bottom();
            },
            (page, false, 0.0),
        );
    harness.run_steps(4);
    let height = harness.state().2 + 8.0;
    harness.set_size(vec2(760.0, height));
    harness.run_steps(4);
    let image = harness.render().expect("render");
    image.save(path).unwrap();
    println!("wrote {}", path.display());
    harness.into_state().0
}
