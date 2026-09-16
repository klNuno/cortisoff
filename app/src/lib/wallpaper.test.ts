import { describe, expect, it } from "vitest";
import { wallpaperDelay, withWallpaperDelay } from "./wallpaper";
import type { Rule } from "./types";

const legacy: Rule = {
  name: "away from the machine", enabled: true, priority: 0,
  when: { when: "idle", value: true },
  set: [{ cap: "wallpaper.playback", value: { bool: true } }],
};

describe("wallpaper settings", () => {
  it("opens legacy plugins and saves an actual pause with the chosen delay", () => {
    expect(wallpaperDelay(legacy)).toBe(300);
    const saved = withWallpaperDelay(legacy, 90);
    expect(wallpaperDelay(saved)).toBe(90);
    expect(saved.set).toEqual([{ cap: "wallpaper.playback", value: { bool: false } }]);
    expect(saved.name).toBe(legacy.name);
    expect(saved.enabled).toBe(true);
    expect(saved.transition).toBeNull();
    expect(legacy.set[0].value).toEqual({ bool: true });
  });
  it("leaves custom conditions in the generic editor", () => {
    expect(wallpaperDelay({ ...legacy, when: { when: "always" } })).toBeNull();
  });
  it("rejects invalid delays", () => {
    for (const seconds of [0, -1, 86401, 1.5, NaN]) {
      expect(() => withWallpaperDelay(legacy, seconds)).toThrow();
    }
  });
});
