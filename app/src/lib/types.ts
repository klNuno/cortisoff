/**
 * The wire, in TypeScript.
 *
 * A hand-written mirror of `cortisoff-ipc`. It is hand-written because the
 * protocol is small and stable and a code generator would be a build step to
 * maintain forever; if a field ever goes missing here, `pnpm check` says so
 * where the field is read.
 */

/** A capability's public name, `display.vibrance` and friends. */
export type CapabilityId = string;

/** Serde tags this one externally, so exactly one key is present. */
export type Value =
  | { int: number }
  | { float: number }
  | { bool: boolean }
  | { text: string }
  | { mode: { width: number; height: number; refresh_mhz: number } };

export type AppRef = {
  exe: string;
  title: string | null;
  pid: number;
  started_at_ms: number;
};

export type LogLevel = "trace" | "info" | "warn" | "error";

export type Event =
  | ({ kind: "app_focus" } & AppRef)
  | { kind: "fullscreen"; exclusive: boolean }
  | { kind: "maximized"; maximized: boolean }
  | { kind: "idle"; idle: boolean }
  | { kind: "session_lock"; locked: boolean }
  | { kind: "display_config_changed" }
  | { kind: "reapply"; cap: CapabilityId }
  | { kind: "power_source"; on_battery: boolean }
  | { kind: "audio_active"; active: boolean }
  | { kind: "time_of_day"; minute: number; second?: number }
  | { kind: "shutdown" };

export type Action =
  | { action: "apply"; cap: CapabilityId; value: Value }
  | { action: "restore_baseline"; cap: CapabilityId }
  | { action: "set_timer"; id: number }
  | { action: "cancel_timer"; id: number }
  | { action: "run"; program: string; args: string[] }
  | { action: "log"; level: LogLevel; message: string };

export type Notification =
  | { notify: "event"; [key: string]: unknown }
  | { notify: "rule_edge"; rule: string; holding: boolean }
  | { notify: "action"; [key: string]: unknown }
  | { notify: "log"; level: LogLevel; message: string }
  | { notify: "closing" };

/**
 * A rule as authored, which is what a card shows and what the settings dialog
 * edits. Adjacently tagged on the Rust side, so a condition is always
 * `{ when, value }` and a unit one carries no value at all.
 */
export type Condition =
  | { when: "always" }
  | { when: "app"; value: { exe: string | null; title_contains: string | null } }
  | { when: "fullscreen"; value: boolean }
  | { when: "maximized"; value: boolean }
  | { when: "idle"; value: boolean }
  | { when: "idle_for"; value: number }
  | { when: "locked"; value: boolean }
  | { when: "on_battery"; value: boolean }
  | { when: "audio_active"; value: boolean }
  | { when: "between"; value: { start: string; end: string } }
  | { when: "all"; value: Condition[] }
  | { when: "any"; value: Condition[] }
  | { when: "not"; value: Condition };

export type Assign = { cap: CapabilityId; value: Value };

/**
 * A fade across the rule's time window instead of a switch at its edges.
 * `from` is where each faded value starts, outside the window; `offset` is 0
 * to 100, 0 putting both fades inside the window and 100 outside it.
 */
export type Transition = {
  seconds: number;
  offset: number;
  from: Assign[];
};

export type Rule = {
  name: string;
  when: Condition;
  priority: number;
  enabled: boolean;
  set: Assign[];
  /** Absent on the wire when a rule switches rather than fades. */
  transition?: Transition | null;
};

/**
 * How a capability is edited, decided by the actuator and reported by the
 * daemon. The dashboard builds a slider, a switch or a text field from this
 * and knows nothing about any capability in particular, so a new one ships
 * with its own control and needs no dashboard change.
 */
export type Control =
  | { kind: "range"; min: number; max: number; step: number; default: number; unit: string | null }
  | { kind: "switch" }
  | { kind: "text"; hint: string };

export type CapabilityReport = {
  id: CapabilityId;
  backend: string | null;
  missing_because: string | null;
  /**
   * Whether the daemon can write to it. `app.focus` and `session.lock` are
   * things the machine tells cortisoff, so an override box on one of them is
   * offering something the daemon will refuse.
   */
  settable: boolean;
  /** What a person calls it. The id stays a hover away. */
  label: string;
  /** Absent for a capability nothing on this machine can write. */
  control: Control | null;
};

export type RuleStateReport =
  | { state: "active" }
  | { state: "disabled" }
  | { state: "unsupported"; missing: CapabilityId[] }
  | { state: "empty" };

export type RuleReport = {
  name: string;
  priority: number;
  state: RuleStateReport;
  holding: boolean;
  sets: CapabilityId[];
};

export type ClaimReport = {
  cap: CapabilityId;
  owner: string;
  priority: number;
  value: Value;
  shadowed: string[];
};

export type WorldReport = {
  app: AppRef | null;
  fullscreen: boolean;
  /** The focused window fills its monitor: maximised, borderless or exclusive. */
  maximized: boolean;
  idle: boolean;
  locked: boolean;
  on_battery: boolean;
  audio_active: boolean;
  minute_of_day: number | null;
};

export type StateReport = {
  active?: boolean;
  world: WorldReport;
  rules: RuleReport[];
  claims: ClaimReport[];
};

export type DoctorReport = {
  version: string;
  /** The daemon's own process, so the window can show what it costs. */
  pid: number;
  capabilities: CapabilityReport[];
  rules: RuleReport[];
  journal_dirty: CapabilityId[];
  warnings: string[];
};

/** What the window shows in place of a value, in the one shape a person reads. */
export function showValue(value: Value): string {
  if ("int" in value) return String(value.int);
  if ("float" in value) return String(value.float);
  if ("bool" in value) return value.bool ? "on" : "off";
  if ("text" in value) return value.text;
  const { width, height, refresh_mhz } = value.mode;
  const size = width && height ? `${width}x${height}` : "";
  const hz = refresh_mhz ? `@${(refresh_mhz / 1000).toString()}` : "";
  return `${size}${hz}` || "unchanged";
}

/** `932` becomes `15:32`, because nobody reads a minute of the day. */
export function showMinute(minute: number | null): string {
  if (minute === null) return "unknown";
  const h = Math.floor(minute / 60);
  const m = minute % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

/**
 * The daemon carries the full image path, because a rule matches on the file
 * name and the path is what identifies the process. A person reading a chip
 * wants the file name, with the path a hover away.
 */
export function exeName(path: string): string {
  const cut = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return cut >= 0 ? path.slice(cut + 1) : path;
}
