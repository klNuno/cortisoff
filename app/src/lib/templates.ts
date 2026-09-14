/**
 * What the `+` card offers.
 *
 * A rule is what this project calls a plugin: there is no plugin runtime and
 * there never will be one, so adding a feature means adding a rule. These are
 * the starting points, each already set to something that works, because a new
 * rule with empty fields teaches nobody what a rule is.
 */
import type { Rule } from "./types";

/** The priority band a focus rule belongs in, matching `cortisoff_core::priority`. */
const FOCUS = 100;

export type Template = {
  id: string;
  title: string;
  /** One line in the picker. Says what it does, not what it is. */
  summary: string;
  /** Capabilities it needs. A machine without them shows it as unavailable. */
  needs: string[];
  make: () => Rule;
};

export const templates: Template[] = [
  {
    id: "blue-light",
    title: "Blue light at night",
    summary:
      "Fades the screen down to 3200 K at night, from wherever it is, and leaves it alone while an app fills the screen.",
    needs: ["display.color_temp"],
    make: () => ({
      name: "blue light at night",
      priority: 0,
      enabled: true,
      when: {
        when: "all",
        value: [
          { when: "between", value: { start: "20:00", end: "07:00" } },
          { when: "not", value: { when: "maximized", value: true } },
        ],
      },
      set: [{ cap: "display.color_temp", value: { int: 3200 } }],
      transition: { seconds: 60, offset: 0, from: [] },
    }),
  },
  {
    id: "vibrance",
    title: "Vibrance in a game",
    summary: "Turns the saturation up while one application is focused, and back down after.",
    needs: ["display.vibrance"],
    make: () => ({
      name: "vibrance in game",
      priority: FOCUS,
      enabled: true,
      when: { when: "app", value: { exe: "cs2.exe", title_contains: null } },
      set: [{ cap: "display.vibrance", value: { int: 100 } }],
    }),
  },
  {
    id: "refresh",
    title: "Refresh rate in a game",
    summary: "Switches every monitor that can do it to a rate while an application is focused.",
    needs: ["display.mode"],
    make: () => ({
      name: "high refresh in game",
      priority: FOCUS,
      enabled: true,
      when: { when: "app", value: { exe: "cs2.exe", title_contains: null } },
      set: [{ cap: "display.mode", value: { mode: { width: 0, height: 0, refresh_mhz: 240000 } } }],
    }),
  },
  {
    id: "away",
    title: "When you walk away",
    summary: "Anything you want done while nobody is at the machine.",
    needs: [],
    make: () => ({
      name: "away from the machine",
      priority: 0,
      enabled: true,
      when: { when: "idle", value: true },
      set: [{ cap: "display.color_temp", value: { int: 2700 } }],
    }),
  },
];

/** A plugin with nothing filled in, for someone who knows what they want. */
export function blankRule(): Rule {
  return {
    name: "new plugin",
    priority: 0,
    enabled: false,
    when: { when: "always" },
    set: [],
  };
}

/** A name nothing else uses, because names are how a rule is pointed at. */
export function uniqueName(wanted: string, taken: string[]): string {
  if (!taken.includes(wanted)) return wanted;
  for (let n = 2; ; n += 1) {
    const candidate = `${wanted} ${n}`;
    if (!taken.includes(candidate)) return candidate;
  }
}
