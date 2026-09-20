import type { CapabilityReport, Rule } from "./types";

/** Recognize the simple pause plugin, including the original boolean idle rule. */
export function wallpaperDelay(rule: Rule): number | null {
  if (rule.set.length !== 1 || rule.set[0].cap !== "wallpaper.playback"
      || !("bool" in rule.set[0].value)) return null;
  if (rule.when.when === "idle" && rule.when.value) return 300;
  if (rule.when.when === "idle_for") return rule.when.value;
  return null;
}

export function wallpaperApp(capabilities: CapabilityReport[]): string {
  const backend = capabilities.find((c) => c.id === "wallpaper.playback")?.backend;
  if (backend?.includes("wallpaper-engine") && backend.includes("lively")) return "Wallpaper Engine + Lively";
  return backend === "lively" ? "Lively" : "Wallpaper Engine";
}

export function withWallpaperDelay(rule: Rule, seconds: number): Rule {
  if (!Number.isInteger(seconds) || seconds < 1 || seconds > 86400) {
    throw new Error("Choose a delay between 1 second and 24 hours.");
  }
  return { ...rule, when: { when: "idle_for", value: seconds },
    set: [{ cap: "wallpaper.playback", value: { bool: false } }], transition: null };
}
