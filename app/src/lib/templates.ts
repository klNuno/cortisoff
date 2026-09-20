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
    id: "display-profile",
    title: "Display profile",
    summary: "Sets saturation, resolution and refresh rate while your application is in the foreground. Restores them when you switch away.",
    needs: ["display.vibrance", "display.mode"],
    make: () => ({
      name: "display profile",
      priority: FOCUS,
      enabled: true,
      when: { when: "app", value: { exe: "cs2.exe", title_contains: null } },
      set: [
        { cap: "display.vibrance", value: { int: 100 } },
        { cap: "display.mode", value: { mode: { width: 2560, height: 1440, refresh_mhz: 240000 } } },
      ],
    }),
  },
  {
    id: "away",
    title: "Wallpaper Engine / Lively",
    summary: "Pause your animated wallpaper after a delay you choose. Resume when you use the keyboard or mouse.",
    needs: ["wallpaper.playback"],
    make: () => ({
      name: "pause wallpaper while away",
      priority: 0,
      enabled: true,
      when: { when: "idle_for", value: 300 },
      set: [{ cap: "wallpaper.playback", value: { bool: false } }],
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
