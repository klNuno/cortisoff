/**
 * Updates, in the background, without asking.
 *
 * Checked once per launch. The download happens on its own and nothing is
 * installed until the person clicks: replacing a running application under
 * someone who is in the middle of something is the one thing an updater must
 * not do.
 *
 * Nothing here runs in `pnpm tauri dev`, where there is no signed bundle and
 * the manifest would answer for a version that is not what is running.
 */
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";

import { toast } from "./toasts.svelte";

type Stage = "idle" | "checking" | "downloading" | "ready" | "installing";

class Updater {
  stage = $state<Stage>("idle");
  version = $state("");

  #pending: Update | null = null;
  #started = false;

  /** The one line the window shows, or null when there is nothing to say. */
  get message(): string | null {
    switch (this.stage) {
      case "ready":
        return this.version ? `Version ${this.version} is ready` : "An update is ready";
      case "installing":
        return "Installing";
      default:
        return null;
    }
  }

  async startInBackground() {
    if (import.meta.env.DEV || this.#started) return;
    this.#started = true;
    this.stage = "checking";

    // Which half failed is the difference between a release that never
    // published its manifest and a payload nobody can download.
    let stage: "check" | "download" = "check";
    try {
      const update = await check();
      if (!update) {
        this.stage = "idle";
        return;
      }
      this.#pending = update;
      this.version = update.version;
      this.stage = "downloading";

      stage = "download";
      await update.download();
      this.stage = "ready";
    } catch (e) {
      console.error(`update ${stage} failed:`, e);
      this.#reset();
    }
  }

  async install() {
    if (this.stage !== "ready" || !this.#pending) return;
    try {
      this.stage = "installing";
      await this.#pending.install();
      await relaunch();
    } catch (e) {
      console.error("installing the update failed:", e);
      this.#reset();
      toast("the update could not be installed", "error");
    }
  }

  #reset() {
    this.#pending = null;
    this.version = "";
    this.stage = "idle";
    this.#started = false;
  }
}

export const updater = new Updater();
