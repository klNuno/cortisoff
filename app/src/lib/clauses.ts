/**
 * A condition, as a list of rows a person can edit.
 *
 * The rule language is a tree, and a tree editor is the wrong thing to put in
 * front of someone who wants their screen to warm up at night. Nearly every
 * real condition is "all of these, and not that", which is a flat list, so
 * that is what the dialog shows.
 *
 * A condition that does not fit the list is not mangled into one: [`toClauses`]
 * answers null and the dialog says the condition was written by hand and leaves
 * it alone. Losing a condition someone wrote is worse than not editing it.
 */
import type { Condition } from "./types";

export type Flag = "maximized" | "fullscreen" | "idle" | "locked" | "on_battery" | "audio_active";

export type Clause =
  | { kind: "app"; not: boolean; apps: string[] }
  | { kind: "title"; not: boolean; text: string }
  | { kind: "flag"; not: boolean; flag: Flag; value: boolean }
  | { kind: "between"; not: boolean; start: string; end: string };

export const flagLabels: Record<Flag, string> = {
  maximized: "an app is fullscreen or maximised",
  fullscreen: "a game is in exclusive fullscreen",
  idle: "you are away",
  locked: "the session is locked",
  on_battery: "on battery",
  audio_active: "something is playing",
};

export function toClauses(condition: Condition): Clause[] | null {
  if (condition.when === "always") return [];
  if (condition.when === "all") {
    const out: Clause[] = [];
    for (const child of condition.value) {
      const clauses = clauseOf(child, false);
      if (!clauses) return null;
      out.push(...clauses);
    }
    return out;
  }
  return clauseOf(condition, false);
}

function clauseOf(condition: Condition, not: boolean): Clause[] | null {
  switch (condition.when) {
    case "not":
      // One level only. `not.not` is a thing nobody writes and a thing this
      // list cannot show.
      return not ? null : clauseOf(condition.value, true);
    case "app": {
      const out: Clause[] = [];
      if (condition.value.exe) out.push({ kind: "app", not, apps: [condition.value.exe] });
      if (condition.value.title_contains) {
        out.push({ kind: "title", not, text: condition.value.title_contains });
      }
      return out.length ? out : null;
    }
    case "any": {
      const apps: string[] = [];
      for (const branch of condition.value) {
        if (branch.when !== "app" || !branch.value.exe || branch.value.title_contains) return null;
        apps.push(branch.value.exe);
      }
      return apps.length ? [{ kind: "app", not, apps }] : null;
    }
    case "between":
      return [{ kind: "between", not, start: condition.value.start, end: condition.value.end }];
    case "maximized":
    case "fullscreen":
    case "idle":
    case "locked":
    case "on_battery":
    case "audio_active":
      return [{ kind: "flag", not, flag: condition.when, value: condition.value }];
    default:
      return null;
  }
}

export function fromClauses(clauses: Clause[]): Condition {
  const parts = clauses.map(conditionOf);
  if (parts.length === 0) return { when: "always" };
  if (parts.length === 1) return parts[0];
  return { when: "all", value: parts };
}

function conditionOf(clause: Clause): Condition {
  const inner = bare(clause);
  return clause.not ? { when: "not", value: inner } : inner;
}

function bare(clause: Clause): Condition {
  switch (clause.kind) {
    case "app": {
      const named = clause.apps.map((exe) => exe.trim()).filter(Boolean);
      if (named.length === 1) return { when: "app", value: { exe: named[0], title_contains: null } };
      return {
        when: "any",
        value: named.map((exe) => ({
          when: "app" as const,
          value: { exe, title_contains: null },
        })),
      };
    }
    case "title":
      return { when: "app", value: { exe: null, title_contains: clause.text } };
    case "flag":
      return { when: clause.flag, value: clause.value };
    case "between":
      return { when: "between", value: { start: clause.start, end: clause.end } };
  }
}

/** A clause that would be dropped on save: an empty app list, a blank title. */
export function isEmpty(clause: Clause): boolean {
  if (clause.kind === "app") return clause.apps.every((a) => !a.trim());
  if (clause.kind === "title") return !clause.text.trim();
  return false;
}

export function newClause(kind: Clause["kind"]): Clause {
  switch (kind) {
    case "app":
      return { kind: "app", not: false, apps: [""] };
    case "title":
      return { kind: "title", not: false, text: "" };
    case "flag":
      // The state most rules are written to stop for, and stopping is what
      // a new row usually means, so it starts negated.
      return { kind: "flag", not: true, flag: "maximized", value: true };
    case "between":
      return { kind: "between", not: false, start: "20:00", end: "07:00" };
  }
}
