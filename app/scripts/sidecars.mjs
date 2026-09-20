// Builds the daemon and the CLI and puts them where Tauri's `externalBin`
// looks: `src-tauri/binaries/<name>-<target triple>`. The installer then ships
// them next to the dashboard, which is where the tray icon and the dashboard
// look for each other.
//
//     node scripts/sidecars.mjs            # debug, for `pnpm tauri dev`
//     node scripts/sidecars.mjs --release  # what `pnpm tauri build` runs

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const release = process.argv.includes("--release");
const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(app, "..");

// The triple of the toolchain that will build the dashboard, which is the one
// Tauri appends when it looks for a sidecar.
const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)?.[1]?.trim();
if (!host) throw new Error("rustc -vV did not name a host triple");

const profile = release ? "release" : "debug";

// Where cargo writes, asked rather than guessed. `<root>/target` is wrong on
// any machine that shares one build cache between checkouts, and the
// environment does not have to say so: a wrapper can set the directory in the
// cargo invocation alone, which is how this failed on a machine with a
// `build-safe` launcher.
const metadata = execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
  cwd: root,
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
});
const target = resolve(JSON.parse(metadata).target_directory);

execFileSync(
  "cargo",
  ["build", "-p", "cortisoff-daemon", "-p", "cortisoff-cli", ...(release ? ["--release"] : [])],
  { cwd: root, stdio: "inherit" },
);

const out = join(app, "src-tauri", "binaries");
mkdirSync(out, { recursive: true });
const ext = process.platform === "win32" ? ".exe" : "";
for (const name of ["cortisoffd", "cortisoff"]) {
  const from = join(target, profile, `${name}${ext}`);
  const to = join(out, `${name}-${host}${ext}`);
  copyFileSync(from, to);
  console.log(`${from} -> ${to}`);
}
