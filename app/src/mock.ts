/**
 * The dashboard with no daemon and no Tauri behind it, for looking at.
 *
 * `pnpm dev` serves it at `/mock.html`. Every command the window sends is
 * answered here from one fixed evening: the blue light rule halfway through
 * its fade, a game rule waiting, one rule switched off. The query string
 * picks what is on screen, and `scripts/capture.mjs` photographs each one. A
 * dialog change is checked by opening a PNG, never by starting a daemon that
 * tints a real screen.
 *
 *     /mock.html?open=settings              the general tab
 *     /mock.html?open=settings:machine      any tab by name
 *     /mock.html?open=add
 *     /mock.html?open=rule:blue light at night
 *     /mock.html?theme=light&usage=1        the light scheme, usage readout on
 *     /mock.html?down                       no daemon running
 */
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { mount } from "svelte";

import "./app.css";
import { daemon } from "$lib/daemon.svelte";
import { templates } from "$lib/templates";
import type {
  CapabilityReport,
  ClaimReport,
  Control,
  DoctorReport,
  Rule,
  RuleReport,
  StateReport,
  Value,
} from "$lib/types";

// Before anything asks for the current window, or the titlebar throws.
mockWindows("main");

const query = new URLSearchParams(location.search);

// The window's own settings, seeded before the settings module reads them.
// No acrylic behind a browser tab, so the ground is painted solid, the way
// the window looks on a platform without a blur.
localStorage.setItem(
  "cortisoff.window",
  JSON.stringify({
    scheme: query.get("theme") ?? "dark",
    acrylic: false,
    usage: query.get("usage") === "1",
  }),
);

let rules: Rule[] = templates.map((t) => t.make());
rules = rules.map((r) => (r.name === "pause wallpaper while away" ? { ...r, enabled: false } : r));
if (query.has("disabled")) rules = rules.map((r) => ({ ...r, enabled: false }));
let autostart = true;

const kelvin: Control = { kind: "range", min: 1500, max: 6600, step: 50, default: 3400, unit: "K" };
const percent: Control = { kind: "range", min: 0, max: 100, step: 1, default: 50, unit: "%" };

/** What a Windows machine with an NVIDIA card and Wallpaper Engine reports. */
const capabilities: CapabilityReport[] = (
  [
    ["app.focus", "winevent", "Focused app", null],
    ["app.fullscreen", "shqueryuns", "Exclusive fullscreen", null],
    ["app.maximized", "winevent", "Maximised window", null],
    ["audio.active", "wasapi", "Sound playing", null],
    ["display.color_temp", "gamma", "Colour temperature", kelvin],
    ["display.config", "wm_displaychange", "Monitor changes", null],
    ["display.mode", "changedisplaysettings", "Display mode", { kind: "text", hint: "2560x1440@240 or @240" }],
    ["display.vibrance", "nvapi", "Vibrance", percent],
    ["power.source", "powersetting", "Power source", null],
    ["session.lock", "wtsnotify", "Session lock", null],
    ["user.idle", "lastinput", "Nobody there", null],
    ["wallpaper.playback", "wallpaper-engine", "Wallpaper playback", { kind: "switch" }],
  ] as [string, string, string, Control | null][]
).map(([id, backend, label, control]) => ({
  id,
  backend,
  missing_because: null,
  settable: control !== null,
  label,
  control,
}));

const holding = new Set(["blue light at night"]);

function reports(): RuleReport[] {
  return rules.map((r) => ({
    name: r.name,
    priority: r.priority,
    state: r.enabled ? { state: "active" } : { state: "disabled" },
    holding: r.enabled && !query.has("inactive") && holding.has(r.name),
    sets: r.set.map((a) => a.cap),
  }));
}

function claims(): ClaimReport[] {
  const blue = rules.find((r) => r.name === "blue light at night");
  if (!blue?.enabled || query.has("inactive")) return [];
  return [
    {
      cap: "display.color_temp",
      owner: blue.name,
      priority: blue.priority,
      value: { int: 4900 },
      shadowed: [],
    },
  ];
}

function state(): StateReport {
  return {
    active: rules.some((r) => r.enabled),
    world: {
      app: {
        exe: "C:\\Program Files\\Mozilla Firefox\\firefox.exe",
        title: "cortisoff",
        pid: 4242,
        started_at_ms: 0,
      },
      fullscreen: false,
      maximized: true,
      idle: false,
      locked: false,
      on_battery: false,
      audio_active: true,
      minute_of_day: 20 * 60 + 20,
    },
    rules: reports(),
    claims: claims(),
  };
}

function doctor(): DoctorReport {
  return {
    version: "0.0.0",
    pid: 8744,
    capabilities,
    rules: reports(),
    journal_dirty: [],
    warnings: [
      "the gamma ramp is clamped by Windows, so the blue light filter looks weak in exclusive fullscreen, and under HDR on a monitor that does not answer DDC/CI. lifting it is one registry value, HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ICM\\GdiIcmGammaRange set to 256, admin once plus a reboot",
    ],
  };
}

/** The daemon's `read_value`, for the shapes the settings dialog sends. */
function readValue(text: string): Value {
  const trimmed = text.trim();
  if (trimmed === "on" || trimmed === "true") return { bool: true };
  if (trimmed === "off" || trimmed === "false") return { bool: false };
  if (/^-?\d+$/.test(trimmed)) return { int: Number(trimmed) };
  return { text: trimmed };
}

mockIPC(
  (cmd, payload) => {
    const args = (payload ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "status":
        return query.has("down")
          ? { phase: "down", reason: "the daemon is not running" }
          : { phase: "connected", reason: null };
      case "state":
        if (query.has("down")) throw "no daemon is listening on this session";
        return state();
      case "doctor":
        if (query.has("down")) throw "no daemon is listening on this session";
        return doctor();
      case "rules":
        if (query.has("down")) throw "no daemon is listening on this session";
        return rules;
      case "save_rules":
        rules = args.rules as Rule[];
        return null;
      case "read_value":
        return readValue(String(args.text ?? ""));
      case "autostart":
        return autostart;
      case "set_autostart":
        autostart = Boolean(args.on);
        return null;
      case "usage":
        return (args.pids as number[]).map((pid) => ({
          pid,
          cpu: pid === 0 ? 0.4 : 0.1,
          rss_bytes: pid === 0 ? 96 * 1_048_576 : 14 * 1_048_576,
        }));
      default:
        // `apply`, `clear`, `preview`, `preview_end`, `reload`, `stop_daemon`
        // and the window plugin all succeed and change nothing.
        return null;
    }
  },
  { shouldMockEvents: true },
);

const { settings } = await import("$lib/settings.svelte");
settings.start();

const { default: App } = await import("./App.svelte");
await daemon.start();

daemon.record({ notify: "event", kind: "time_of_day", minute: 20 * 60 });
daemon.record({ notify: "rule_edge", rule: "blue light at night", holding: true });
daemon.record({ notify: "action", action: "apply", cap: "display.color_temp" });
daemon.record({
  notify: "event",
  kind: "app_focus",
  exe: "C:\\Program Files\\Mozilla Firefox\\firefox.exe",
  title: "cortisoff",
});
daemon.record({ notify: "event", kind: "audio_active", active: true });

mount(App, { target: document.getElementById("app")! });

function find(selector: string, text?: string): HTMLButtonElement | undefined {
  const all = [...document.querySelectorAll<HTMLButtonElement>(selector)];
  return text ? all.find((b) => b.textContent?.includes(text)) : all[0];
}

/** Clicks what a person would click to reach the view `?open=` names. */
async function open(what: string) {
  const [head, tail] = what.split(":", 2);
  const steps: (() => HTMLButtonElement | undefined)[] =
    head === "rule"
      ? [() => find(`button[aria-label="Settings for ${tail}"]`)]
      : head === "add"
        ? [() => find("button", "Add")]
        : head === "settings"
          ? [
              () => find('button[aria-label="Settings"]'),
              ...(tail ? [() => find(`button[data-tab="${tail}"]`)] : []),
            ]
          : [];
  for (const step of steps) {
    let clicked = false;
    for (let tries = 0; tries < 50 && !clicked; tries += 1) {
      const target = step();
      if (target) {
        target.click();
        clicked = true;
      } else {
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    }
    if (!clicked) {
      document.documentElement.dataset.ready = `missing:${what}`;
      return;
    }
  }
  document.documentElement.dataset.ready = what;
}

const wanted = query.get("open");
if (wanted) void open(wanted);
else document.documentElement.dataset.ready = "grid";
