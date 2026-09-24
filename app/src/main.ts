import { invoke } from "@tauri-apps/api/core";
import { mount } from "svelte";

import App from "./App.svelte";
import "./app.css";
import { daemon } from "$lib/daemon.svelte";
import { opening } from "$lib/opening";
import { settings } from "$lib/settings.svelte";
import { updater } from "$lib/updater.svelte";

/**
 * How long the window waits for the daemon's first answer before showing
 * itself anyway. A running daemon answers in a few milliseconds, so this only
 * matters when it is not running, and then the window shows it connecting.
 */
const FIRST_ANSWER_MS = 400;

// The scheme and the backdrop are applied before the first frame: the
// backdrop lands a frame later, which is invisible, but awaiting it would
// show a white flash.
settings.start();
const started = daemon.start();
// Checked once per launch, downloaded on its own, installed only when the
// person clicks. Does nothing in dev, where there is no signed bundle.
void updater.startInBackground();

const app = mount(App, { target: document.getElementById("app")! });

// The window is built hidden and shown here, once the page has painted with
// the daemon's answer in it, so the first thing on screen is the grid rather
// than an empty frame that fills a moment later.
await Promise.race([started, delay(FIRST_ANSWER_MS)]);
// Two frames is layout done and painted. A hidden window may not be given
// frames at all, so a short timer stands in for them.
await Promise.race([
  new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  delay(60),
]);
void invoke("reveal").catch(() => {});
opening.shown = true;

function delay(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export default app;
