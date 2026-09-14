/**
 * What the window remembers about itself.
 *
 * Three things, all cosmetic, all kept in the webview's own storage: the
 * colour scheme, whether the window is translucent, and whether the titlebar
 * shows what cortisoff costs. None of it reaches the daemon or the rule file,
 * because none of it changes what a rule does.
 *
 * The scheme is resolved here rather than in CSS: the acrylic tint has to
 * follow it, and Tauri's `setEffects` takes a colour, not a media query.
 */
import { applyBackdrop } from "./backdrop";

export type Scheme = "system" | "light" | "dark";
export type Resolved = "light" | "dark";

const KEY = "cortisoff.window";

type Stored = {
  scheme: Scheme;
  acrylic: boolean;
  usage: boolean;
};

const DEFAULTS: Stored = { scheme: "system", acrylic: true, usage: false };

function load(): Stored {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<Stored>;
    return {
      scheme: parsed.scheme === "light" || parsed.scheme === "dark" ? parsed.scheme : "system",
      acrylic: parsed.acrylic ?? DEFAULTS.acrylic,
      usage: parsed.usage ?? DEFAULTS.usage,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

class Settings {
  #stored = $state<Stored>(load());
  /** What the OS asks for, tracked live so "system" follows a change of theme. */
  #osLight = $state(false);
  #started = false;

  get scheme(): Scheme {
    return this.#stored.scheme;
  }
  get acrylic(): boolean {
    return this.#stored.acrylic;
  }
  get usage(): boolean {
    return this.#stored.usage;
  }

  /** The scheme the page is painted in, with "system" already decided. */
  get resolved(): Resolved {
    if (this.#stored.scheme === "system") return this.#osLight ? "light" : "dark";
    return this.#stored.scheme;
  }

  /**
   * Paint the page and the window from what is stored, and keep painting it
   * as things change. Called once, before the first frame, so the window
   * never flashes the wrong scheme.
   */
  start() {
    if (this.#started) return;
    this.#started = true;
    const query = window.matchMedia("(prefers-color-scheme: light)");
    this.#osLight = query.matches;
    query.addEventListener("change", (e) => (this.#osLight = e.matches));
    $effect.root(() => {
      $effect(() => {
        const resolved = this.resolved;
        const acrylic = this.#stored.acrylic;
        document.documentElement.dataset.theme = resolved;
        document.documentElement.classList.toggle("acrylic", acrylic);
        void applyBackdrop(resolved, acrylic);
      });
    });
  }

  setScheme(scheme: Scheme) {
    this.#save({ ...this.#stored, scheme });
  }
  setAcrylic(acrylic: boolean) {
    this.#save({ ...this.#stored, acrylic });
  }
  setUsage(usage: boolean) {
    this.#save({ ...this.#stored, usage });
  }

  #save(next: Stored) {
    this.#stored = next;
    try {
      localStorage.setItem(KEY, JSON.stringify(next));
    } catch {
      // A webview with storage disabled still gets the change for this run.
    }
  }
}

export const settings = new Settings();
