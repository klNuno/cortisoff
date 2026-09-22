/**
 * Whether the window has been shown yet.
 *
 * The window appears already painted, so what is on screen at that moment
 * must not then fade in: that played every entrance animation at once, a
 * quarter second of grey after the window had opened. Read once, when a
 * component is created. A card added later still enters, and one present at
 * the opening does not.
 */
export const opening = { shown: false };

/** The entrance class for something created now. */
export function entrance(name: "page-in" | "card-in"): string {
  return opening.shown ? name : "";
}
