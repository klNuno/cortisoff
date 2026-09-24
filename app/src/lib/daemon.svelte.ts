/**
 * The one place that talks to the daemon.
 *
 * Everything on screen comes from here, and nothing here knows what a rule
 * means: the daemon decides, this reports. Two channels feed it. `invoke`
 * asks a question and gets one answer, and a subscribed connection pushes
 * notifications as they happen, which is what makes the window live rather
 * than a page you refresh.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { toast } from "./toasts.svelte";
import { exeName, type DoctorReport, type Notification, type Rule, type StateReport, type Value } from "./types";

/** One line of the trace, already turned into something a person reads. */
export type TraceLine = {
  id: number;
  at: string;
  kind: "event" | "hold" | "release" | "action" | "log" | "closing";
  text: string;
  level?: string;
  /** How many times in a row this same line arrived. A fade is dozens. */
  count: number;
};

/**
 * Where the connection stands. `connecting` is the window's first second and
 * the daemon being started for it, and it is not an error: saying "not
 * running" in red there was a lie the window told at every launch.
 */
export type Phase = "connecting" | "connected" | "down";

type Status = { phase: Phase; reason: string | null };

/** Enough to scroll back through a session without holding a day of it. */
const MAX_TRACE = 600;

/**
 * At most one state fetch per this long while notifications stream. A
 * throttle, not a debounce: a fade sends an action every few dozen
 * milliseconds, and a debounce restarted by each of them never fired, so
 * the cards showed the world from before the fade until it ended.
 */
const REFRESH_EVERY_MS = 100;

/** What a notification needs fetched again, cheapest first. */
type Need = 0 | 1 | 2;
const NOTHING: Need = 0;
const STATE: Need = 1;
const EVERYTHING: Need = 2;

class Daemon {
  phase = $state<Phase>("connecting");
  /** A sentence for a person while down. */
  reason = $state<string | null>(null);
  // Raw: every one of these is replaced whole by the next answer and never
  // edited in place, and a deep proxy over a doctor report cost a walk of the
  // whole object on every refresh.
  state = $state.raw<StateReport | null>(null);
  doctor = $state.raw<DoctorReport | null>(null);
  /**
   * The rules as authored, which is what the cards edit. Separate from
   * `state.rules`, which says what this machine did with them.
   */
  rules = $state.raw<Rule[]>([]);
  trace = $state.raw<TraceLine[]>([]);
  /** The last thing that went wrong, shown until the next action succeeds. */
  error = $state<string | null>(null);
  busy = $state(false);

  #nextId = 1;
  #need: Need = NOTHING;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #fetching = false;
  #announced = false;

  get connected(): boolean {
    return this.phase === "connected";
  }

  async start() {
    await Promise.all([
      listen<Status>("daemon://status", (e) => this.#status(e.payload)),
      listen<Notification>("daemon://notification", (e) => {
        this.record(e.payload);
        this.#want(needs(e.payload));
      }),
    ]);

    // The watcher may have spoken before anything listened. It only speaks on
    // a change, so the current phase is asked for rather than waited for.
    const [status] = await Promise.all([
      invoke<Status | null>("status").catch(() => null),
      this.refresh(),
    ]);
    if (status && !this.connected) this.#status(status);
  }

  #status(status: Status) {
    const was = this.phase;
    this.phase = status.phase;
    this.reason = status.reason;
    // The refresh is what says "connected" in the trace, once, whichever of
    // the two learns it first.
    if (status.phase === "connected" && was !== "connected") void this.refresh();
    if (status.phase === "down") {
      this.#announced = false;
      this.state = null;
      this.doctor = null;
      this.rules = [];
    }
  }

  /** Every report at once: they are three questions about the same instant. */
  async refresh() {
    try {
      const [state, doctor, rules] = await Promise.all([
        invoke<StateReport>("state"),
        invoke<DoctorReport>("doctor"),
        invoke<Rule[]>("rules"),
      ]);
      this.state = state;
      this.doctor = doctor;
      this.rules = rules;
      if (!this.#announced) this.push("log", "connected to the daemon");
      this.#announced = true;
      this.phase = "connected";
      this.reason = null;
    } catch {
      // Not an error to show: the watcher thread owns the connection's
      // phase and says why it is down.
    }
  }

  /** What changes on every event: who holds what, and what the machine sees. */
  async refreshState() {
    try {
      this.state = await invoke<StateReport>("state");
    } catch {
      // Same as above.
    }
  }

  /** Ask for a fetch, merged with whatever is already waiting. */
  #want(need: Need) {
    if (need > this.#need) this.#need = need;
    if (this.#need === NOTHING || this.#timer !== null || this.#fetching) return;
    this.#timer = setTimeout(() => {
      this.#timer = null;
      void this.#fetch();
    }, REFRESH_EVERY_MS);
  }

  async #fetch() {
    const need = this.#need;
    this.#need = NOTHING;
    if (!this.connected) return;
    this.#fetching = true;
    try {
      await (need === EVERYTHING ? this.refresh() : this.refreshState());
    } finally {
      this.#fetching = false;
      // Whatever arrived during the fetch gets the next slot.
      this.#want(NOTHING);
    }
  }

  /** A manual override written as text, the way the CLI takes it. */
  async set(cap: string, value: string) {
    return this.run(() => invoke("set", { cap, value }), `${cap} is yours until you release it`);
  }

  /**
   * The same override, from a value that is already structured. A card knows
   * exactly what its rule sets, so it has no reason to render that to text and
   * ask the daemon to read it back.
   */
  async apply(cap: string, value: Value) {
    return this.run(() => invoke("apply", { cap, value }), `${cap} is yours until you release it`);
  }

  async clear(cap: string) {
    return this.run(() => invoke("clear", { cap }), `${cap} is back to the plugins`);
  }

  /**
   * Show a value while a slider is dragged. Not `run`: a drag sends a
   * value every few dozen milliseconds, and a refresh or a toast per value
   * would be the freeze the sliders exist to avoid. The daemon holds it
   * above everything else until `previewEnd`, then fades back.
   */
  async preview(cap: string, value: Value) {
    try {
      await invoke("preview", { cap, value });
    } catch (e) {
      this.error = typeof e === "string" ? e : String(e);
    }
  }

  async previewEnd(cap: string) {
    try {
      await invoke("preview_end", { cap });
    } catch (e) {
      this.error = typeof e === "string" ? e : String(e);
    }
  }

  /**
   * Write the whole set and load it. Switching a rule off, editing it, adding
   * one and deleting one are all this call, which is why nothing has to agree
   * on a rule's identity beyond its position in this array.
   */
  async saveRules(rules: Rule[], done: string) {
    return this.run(() => invoke("save_rules", { rules }), done);
  }

  async reload() {
    return this.run(() => invoke("reload"), "plugins re-read from the file");
  }

  /** The watcher connects to it on its own once its pipe is open. */
  async startDaemon() {
    return this.run(() => invoke("start_daemon"));
  }

  /** Stops the daemon, not this window. Every baseline goes back on the way out. */
  async stopDaemon() {
    return this.run(() => invoke("stop_daemon"), "the daemon stopped, every screen put back");
  }

  /**
   * The daemon refusing is not a failure of the window. Its message was
   * written to be read by a person, so it is shown unchanged.
   *
   * Answers whether it worked, because a dialog should stay open on a refusal
   * rather than closing over the reason it was refused.
   */
  async run(call: () => Promise<unknown>, done?: string): Promise<boolean> {
    this.busy = true;
    this.error = null;
    try {
      await call();
      await this.refresh();
      if (done) toast(done);
      return true;
    } catch (e) {
      this.error = typeof e === "string" ? e : String(e);
      toast(this.error, "error");
      return false;
    } finally {
      this.busy = false;
    }
  }

  record(n: Notification) {
    switch (n.notify) {
      case "event":
        this.push("event", describeEvent(n));
        break;
      case "rule_edge":
        this.push(n.holding ? "hold" : "release", n.rule);
        break;
      case "action":
        this.push("action", describeAction(n));
        break;
      case "log":
        this.push("log", n.message, n.level);
        break;
      case "closing":
        this.push("closing", "the daemon is going down");
        break;
    }
  }

  push(kind: TraceLine["kind"], text: string, level?: string) {
    const now = new Date();
    const at = [now.getHours(), now.getMinutes(), now.getSeconds()]
      .map((n) => String(n).padStart(2, "0"))
      .join(":");
    const last = this.trace.at(-1);
    if (last && last.kind === kind && last.text === text && last.level === level) {
      // A fade is one line with a count, not a screen of the same line.
      this.trace = [...this.trace.slice(0, -1), { ...last, at, count: last.count + 1 }];
      return;
    }
    const line: TraceLine = { id: this.#nextId++, at, kind, text, level, count: 1 };
    this.trace =
      this.trace.length >= MAX_TRACE ? [...this.trace.slice(1 - MAX_TRACE), line] : [...this.trace, line];
  }

  clearTrace() {
    this.trace = [];
  }
}

/**
 * An event or an action changes who holds what, which is the state report.
 * An edge or a log can mean the rule set itself changed under the window,
 * reloaded from the file or refused, so those fetch everything.
 */
function needs(n: Notification): Need {
  switch (n.notify) {
    case "event":
    case "action":
      return STATE;
    case "rule_edge":
    case "log":
      return EVERYTHING;
    case "closing":
      return NOTHING;
  }
}

function describeEvent(n: Record<string, unknown>): string {
  switch (n.kind) {
    case "app_focus":
      return `focus ${exeName(String(n.exe))}${n.title ? ` (${n.title})` : ""}`;
    case "fullscreen":
      return n.exclusive ? "exclusive fullscreen taken" : "exclusive fullscreen released";
    case "maximized":
      return n.maximized ? "the focused window fills the screen" : "the focused window is windowed";
    case "idle":
      return n.idle ? "user went away" : "user came back";
    case "session_lock":
      return n.locked ? "session locked" : "session unlocked";
    case "display_config_changed":
      return "the display configuration changed";
    case "reapply":
      return `${n.cap} has to be pushed again`;
    case "power_source":
      return n.on_battery ? "on battery" : "on mains";
    case "audio_active":
      return n.active ? "audio started" : "audio stopped";
    case "time_of_day": {
      const minute = Number(n.minute ?? 0);
      const h = String(Math.floor(minute / 60)).padStart(2, "0");
      const m = String(minute % 60).padStart(2, "0");
      return `the time is ${h}:${m}`;
    }
    case "shutdown":
      return "shutdown";
    default:
      return String(n.kind ?? "unknown event");
  }
}

function describeAction(n: Record<string, unknown>): string {
  switch (n.action) {
    case "apply":
      return `apply ${n.cap}`;
    case "restore_baseline":
      return `restore ${n.cap}`;
    case "set_timer":
      return "a timer was armed";
    case "cancel_timer":
      return "a timer was cancelled";
    case "run":
      return `run ${n.program}`;
    case "log":
      return String(n.message ?? "");
    default:
      return String(n.action ?? "unknown action");
  }
}

export const daemon = new Daemon();
