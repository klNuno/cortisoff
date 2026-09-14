<script lang="ts">
  import { toastLifetime, type ToastKind } from "$lib/toasts.svelte";

  /**
   * One message, with its own countdown.
   *
   * Hovering pauses it, because the one message worth reading twice is the one
   * that went past while you were looking elsewhere.
   */
  let {
    message,
    kind,
    resetKey,
    ondone,
  }: {
    message: string;
    kind: ToastKind;
    /** Bumped when the same message comes back, which restarts the countdown. */
    resetKey: number;
    ondone: () => void;
  } = $props();

  let timer: ReturnType<typeof setTimeout> | undefined;
  let remaining = 0;
  let startedAt = 0;
  let hovered = false;

  $effect(() => {
    void resetKey;
    void kind;
    remaining = toastLifetime(kind);
    if (!hovered) start();
    return stop;
  });

  function start() {
    startedAt = Date.now();
    timer = setTimeout(ondone, remaining);
  }

  function stop() {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }

  function onmouseenter() {
    hovered = true;
    if (timer === undefined) return;
    stop();
    remaining = Math.max(0, remaining - (Date.now() - startedAt));
  }

  function onmouseleave() {
    hovered = false;
    if (remaining <= 0) ondone();
    else start();
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions (hover only pauses the timer) -->
<div
  class="card-in pointer-events-auto flex max-w-sm items-start gap-2 rounded-card border
    border-line bg-surface px-3 py-2 shadow-xl"
  {onmouseenter}
  {onmouseleave}
>
  <span
    class="mt-1 size-1.5 shrink-0 rounded-full {kind === 'error' ? 'bg-danger' : 'bg-success'}"
  ></span>
  <p class="min-w-0 flex-1 text-[13px] text-foreground">{message}</p>
  <button
    type="button"
    aria-label="Dismiss"
    class="-mr-1 grid size-4 shrink-0 place-items-center rounded text-foreground-subtle
      transition-colors hover:text-foreground"
    onclick={ondone}
  >
    <svg width="8" height="8" viewBox="0 0 10 10" aria-hidden="true">
      <path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1.6" />
    </svg>
  </button>
</div>
