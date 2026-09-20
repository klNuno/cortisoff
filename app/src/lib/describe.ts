/**
 * Rules, in English.
 *
 * A card has one line to say when a rule fires and one to say what it does,
 * and both come from data the daemon sent. This is the only file allowed to
 * put words on that data, so a phrasing changes in one place.
 */
import { withUnit } from "./ranges";
import type { CapabilityReport, Condition, Control, Rule, Transition, Value } from "./types";
import { exeName } from "./types";

/** What a rule's condition says, as one sentence. */
export function describeCondition(condition: Condition): string {
  const clauses = clausesOf(condition);
  if (clauses.length === 0) return "all the time";
  return clauses.join(", ");
}

function clausesOf(condition: Condition): string[] {
  switch (condition.when) {
    case "always":
      return [];
    case "all":
      return condition.value.flatMap(clausesOf);
    case "any": {
      const parts = condition.value.flatMap(clausesOf);
      // `app = ["a.exe", "b.exe"]` arrives as an `any` of two app conditions,
      // and reads as one clause rather than two.
      const apps = appNames(condition);
      if (apps) return [`while ${list(apps)} is focused`];
      return parts.length ? [parts.join(" or ")] : [];
    }
    case "not": {
      const apps = appNames(condition.value);
      if (apps) return [`unless ${list(apps)} is focused`];
      // "unless an app is fullscreen", never "unless while an app is".
      const inner = clausesOf(condition.value).map((c) => c.replace(/^while /, ""));
      return inner.length ? [`unless ${inner.join(" and ")}`] : [];
    }
    case "app": {
      const parts: string[] = [];
      if (condition.value.exe) parts.push(`while ${exeName(condition.value.exe)} is focused`);
      if (condition.value.title_contains) {
        parts.push(`while the title contains "${condition.value.title_contains}"`);
      }
      return parts;
    }
    case "between":
      return [`from ${condition.value.start} to ${condition.value.end}`];
    case "maximized":
      return [
        condition.value
          ? "while an app is fullscreen or maximised"
          : "while no app is fullscreen or maximised",
      ];
    case "fullscreen":
      return [
        condition.value
          ? "while a game is in exclusive fullscreen"
          : "while no game is in exclusive fullscreen",
      ];
    case "idle_for":
      return [`after ${describeDuration(condition.value)} without activity`];
    case "idle":
      return [condition.value ? "while you are away" : "while you are here"];
    case "locked":
      return [condition.value ? "while the session is locked" : "while the session is unlocked"];
    case "on_battery":
      return [condition.value ? "on battery" : "on mains"];
    case "audio_active":
      return [condition.value ? "while something is playing" : "while nothing is playing"];
    default:
      return [];
  }
}

/** The executables an `any` of app conditions names, or null if it is not one. */
function appNames(condition: Condition): string[] | null {
  const branches =
    condition.when === "any" ? condition.value : condition.when === "app" ? [condition] : null;
  if (!branches || branches.length === 0) return null;
  const names: string[] = [];
  for (const branch of branches) {
    if (branch.when !== "app" || !branch.value.exe || branch.value.title_contains) return null;
    names.push(exeName(branch.value.exe));
  }
  return names;
}

function list(items: string[]): string {
  if (items.length <= 1) return items[0] ?? "";
  return `${items.slice(0, -1).join(", ")} or ${items[items.length - 1]}`;
}

/**
 * A value with its unit. `3400` alone means nothing on a card, and the unit
 * comes from the control the daemon reported for the capability, so the card
 * reads right for a capability this file has never heard of.
 */
export function describeValue(value: Value, control?: Control | null): string {
  const unit = control?.kind === "range" ? control.unit : null;
  if ("int" in value) return withUnit(value.int, unit);
  if ("float" in value) return withUnit(value.float, unit);
  if ("bool" in value) return value.bool ? "on" : "off";
  if ("text" in value) return value.text;

  const { width, height, refresh_mhz } = value.mode;
  const size = width && height ? `${width}x${height}` : "";
  const hz = refresh_mhz ? `${refresh_mhz / 1000} Hz` : "";
  if (size && hz) return `${size} at ${hz}`;
  return size || hz || "unchanged";
}

/**
 * A value as the rule file spells it, which is what the settings dialog edits
 * and what `cortisoff set` takes. The daemon parses it back, so this and
 * `cortisoff_ipc::parse_value` are the two halves of one format.
 */
export function valueText(value: Value): string {
  if ("int" in value) return String(value.int);
  if ("float" in value) return String(value.float);
  if ("bool" in value) return value.bool ? "true" : "false";
  if ("text" in value) return value.text;

  const { width, height, refresh_mhz } = value.mode;
  const size = width && height ? `${width}x${height}` : "";
  const hz = refresh_mhz ? `@${refresh_mhz / 1000}` : "";
  return `${size}${hz}`;
}

/** `2400` becomes `40 min`, `5400` becomes `1 h 30 min`. */
export function describeDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  const parts: string[] = [];
  if (h) parts.push(`${h} h`);
  if (m) parts.push(`${m} min`);
  if (s || parts.length === 0) parts.push(`${s} s`);
  return parts.join(" ");
}

/** How a rule fades, as one line, or null for a rule that switches. */
export function describeTransition(transition: Transition | null | undefined): string | null {
  if (!transition) return null;
  const where =
    transition.offset === 0
      ? "inside the window"
      : transition.offset === 100
        ? "outside the window"
        : `${transition.offset}% outside the window`;
  return `fades over ${describeDuration(transition.seconds)}, ${where}`;
}

/**
 * What one capability of a rule does, with where it fades from when it fades:
 * `6600 to 3200 K` rather than a bare `3200 K`. Two whole numbers share the
 * unit once, because a card has about thirty characters for the line.
 */
export function describeAssign(rule: Rule, cap: string, value: Value, control?: Control | null): string {
  const from = rule.transition?.from.find((f) => f.cap === cap);
  const to = describeValue(value, control);
  if (!from) return to;
  if ("int" in from.value && "int" in value) return `${from.value.int} to ${to}`;
  return `${describeValue(from.value, control)} to ${to}`;
}

/** The report for one capability, from the list the daemon sent. */
export function capabilityOf(
  cap: string,
  capabilities: CapabilityReport[],
): CapabilityReport | undefined {
  return capabilities.find((c) => c.id === cap);
}

/**
 * What a person calls a capability, as the daemon reports it. A capability
 * the daemon has not named, or a rule naming one this machine lacks, falls
 * back to the id with its dots read as spaces.
 */
export function capabilityLabel(cap: string, capabilities: CapabilityReport[]): string {
  return capabilityOf(cap, capabilities)?.label ?? cap.replace(/[._]/g, " ");
}
