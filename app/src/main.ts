import { mount } from "svelte";

import App from "./App.svelte";
import "./app.css";
import { daemon } from "$lib/daemon.svelte";
import { settings } from "$lib/settings.svelte";
import { updater } from "$lib/updater.svelte";

// The scheme and the backdrop are applied before the first frame: the
// backdrop lands a frame later, which is invisible, but awaiting it would
// show a white flash.
settings.start();
void daemon.start();
// Checked once per launch, downloaded on its own, installed only when the
// person clicks. Does nothing in dev, where there is no signed bundle.
void updater.startInBackground();

export default mount(App, { target: document.getElementById("app")! });
