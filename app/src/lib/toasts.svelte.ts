/**
 * Short-lived messages, bottom right.
 *
 * The window says what is true right now; a toast says what just happened. An
 * identical message already on screen restarts its own timer rather than
 * stacking, because a button clicked twice is one event to a person.
 */
export type ToastKind = "info" | "error";

export type Toast = {
  id: number;
  message: string;
  kind: ToastKind;
  /** Bumped when the same message comes back, which restarts the countdown. */
  resetKey: number;
};

const MAX = 4;
const LIFETIME = { info: 2600, error: 6000 } as const;

let toasts = $state<Toast[]>([]);
let nextId = 1;

export function allToasts(): Toast[] {
  return toasts;
}

export function toast(message: string, kind: ToastKind = "info") {
  const existing = toasts.find((t) => t.message === message);
  if (existing) {
    existing.kind = kind;
    existing.resetKey += 1;
    return;
  }
  toasts.push({ id: nextId++, message, kind, resetKey: 0 });
  if (toasts.length > MAX) toasts.splice(0, toasts.length - MAX);
}

export function dismissToast(id: number) {
  const at = toasts.findIndex((t) => t.id === id);
  if (at !== -1) toasts.splice(at, 1);
}

export function toastLifetime(kind: ToastKind): number {
  return LIFETIME[kind];
}
