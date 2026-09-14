import { Effect, getCurrentWindow } from "@tauri-apps/api/window";

/**
 * The translucent window.
 *
 * Acrylic is the only DWM backdrop that works with a transparent window:
 * Mica and Tabbed both render black on a layered one. Windows' default
 * acrylic tint is a mid grey that reads plastic, so the tint is forced to a
 * heavy black on the dark scheme and a heavy white on the light one, and the
 * page's own surfaces do the rest.
 *
 * `Effect.UnderWindowBackground` is listed second for macOS: `setEffects`
 * applies the first effect the platform supports, so one call covers both.
 * Linux has no cross-compositor blur protocol, the call no-ops, and the page
 * paints a solid window instead.
 *
 * With acrylic off the effects are cleared and the ground goes opaque, which
 * is also what happens when the platform cannot blur: the `acrylic` class on
 * the root is what the CSS reads, and it is dropped on failure.
 */
export async function applyBackdrop(scheme: "light" | "dark", acrylic: boolean): Promise<boolean> {
  const root = document.documentElement;
  try {
    const win = getCurrentWindow();
    if (!acrylic) {
      await win.clearEffects();
      return true;
    }
    await win.setEffects({
      effects: [Effect.Acrylic, Effect.UnderWindowBackground],
      color: scheme === "light" ? [255, 255, 255, 150] : [0, 0, 0, 150],
    });
    return true;
  } catch {
    root.classList.remove("acrylic");
    return false;
  }
}
