// Photographs the mocked dashboard, one PNG per view, with no daemon running
// and no screen touched.
//
//     node scripts/capture.mjs                     # every view, into captures/
//     node scripts/capture.mjs settings rule-blue   # just those
//
// Starts Vite on its own port, points a headless Edge at `mock.html?open=<view>`
// once per view, and stops both as process trees on the way out, error or not.
// `SIZE=940,1200` photographs a taller window, to see what a dialog scrolls to.
// Edge renders on the GPU through ANGLE's D3D11 backend: SwiftShader would put
// the whole page on the CPU. `EDGE` overrides where the browser is.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(app, "captures");
const port = 1431;
// The window's own inner size, from `src-tauri/src/main.rs`.
const size = process.env.SIZE ?? "940,680";

// Each view is the query string `mock.html` reads.
const views = {
  grid: "",
  "grid-inactive": "inactive=1",
  "grid-disabled": "disabled=1",
  "grid-light": "theme=light&usage=1",
  "grid-down": "down=1",
  settings: "open=settings",
  "settings-machine": "open=settings:machine",
  "settings-logs": "open=settings:logs",
  "settings-light": "theme=light&open=settings",
  add: "open=add",
  "rule-blue-light": "open=rule:blue light at night",
  "rule-display-profile": "open=rule:display profile",
  "rule-wallpaper": "open=rule:pause wallpaper while away",
  "rule-wallpaper-light": "theme=light&open=rule:pause wallpaper while away",
};

const edge =
  process.env.EDGE ??
  [
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
  ].find((path) => existsSync(path));
if (!edge) throw new Error("no Edge found, set EDGE to a Chromium executable");

const wanted = process.argv.slice(2);
const picked = Object.entries(views).filter(
  ([name]) => wanted.length === 0 || wanted.some((w) => name.startsWith(w) || name.includes(w)),
);
if (picked.length === 0) throw new Error(`no view matches ${wanted.join(", ")}`);

/** Windows keeps a child's children alive when the child dies, so the tree goes. */
function killTree(child) {
  if (!child?.pid || child.exitCode !== null) return;
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/T", "/F", "/PID", String(child.pid)], { stdio: "ignore" });
  } else {
    child.kill("SIGKILL");
  }
}

async function waitForServer(url, child) {
  for (let tries = 0; tries < 150; tries += 1) {
    if (child.exitCode !== null) throw new Error(`vite exited with ${child.exitCode}`);
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      // Not listening yet.
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`vite never answered on ${url}`);
}

function run(child, timeoutMs) {
  return new Promise((resolveRun) => {
    const timer = setTimeout(() => {
      killTree(child);
      resolveRun("timeout");
    }, timeoutMs);
    child.on("exit", (code) => {
      clearTimeout(timer);
      resolveRun(code);
    });
  });
}

mkdirSync(out, { recursive: true });
const vite = spawn(
  process.execPath,
  [join(app, "node_modules", "vite", "bin", "vite.js"), "--port", String(port), "--strictPort"],
  { cwd: app, stdio: "ignore", windowsHide: true },
);
const profile = mkdtempSync(join(tmpdir(), "cortisoff-capture-"));
let failed = 0;

try {
  const base = `http://localhost:${port}/mock.html`;
  await waitForServer(base, vite);

  for (const [name, open] of picked) {
    const file = join(out, `${name}.png`);
    rmSync(file, { force: true });
    const url = open ? `${base}?${open.replace(/ /g, "%20")}` : base;
    const browser = spawn(
      edge,
      [
        "--headless=new",
        "--use-gl=angle",
        "--use-angle=d3d11",
        "--mute-audio",
        "--hide-scrollbars",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        `--user-data-dir=${profile}`,
        `--window-size=${size}`,
        // Long enough for the module graph, the first IPC round and the
        // 260 ms card entrance, measured in virtual time so a slow machine
        // does not photograph a half-drawn dialog.
        "--virtual-time-budget=4000",
        `--screenshot=${file}`,
        url,
      ],
      { stdio: "ignore", windowsHide: true },
    );
    const code = await run(browser, 30_000);
    killTree(browser);
    const ok = existsSync(file) && statSync(file).size > 0;
    if (!ok) failed += 1;
    console.log(`${ok ? "ok  " : "FAIL"} ${name} -> ${file}${ok ? "" : ` (edge ${code})`}`);
  }
} finally {
  killTree(vite);
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}

process.exit(failed ? 1 : 0);
