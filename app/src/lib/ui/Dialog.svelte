<script lang="ts">
  import type { Snippet } from "svelte";

  /**
   * Everything that is not the card grid.
   *
   * One shape for all of them, because a window this small cannot afford a
   * second visual language. Escape closes, the backdrop closes, and the panel
   * itself never does: a click inside must not fall through to the backdrop.
   */
  let {
    title,
    subtitle,
    onclose,
    footer,
    children,
  }: {
    title: string;
    subtitle?: string;
    onclose: () => void;
    footer?: Snippet;
    children: Snippet;
  } = $props();

  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window {onkeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events (Escape is handled above,
     and the backdrop is a convenience rather than the only way out) -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="page-in absolute inset-0 z-20 flex items-center justify-center bg-black/40 p-4"
  onclick={onclose}
>
  <div
    class="card-in flex max-h-full w-full max-w-xl flex-col overflow-hidden rounded-xl
      border border-line bg-surface shadow-(--shadow-dialog)"
    role="dialog"
    aria-modal="true"
    aria-label={title}
    tabindex="-1"
    onclick={(event) => event.stopPropagation()}
  >
    <header class="flex shrink-0 items-start gap-3 border-b border-line px-4 py-3">
      <div class="min-w-0 flex-1">
        <h2 class="truncate text-sm font-semibold tracking-tight">{title}</h2>
        {#if subtitle}
          <p class="mt-0.5 text-xs text-foreground-muted">{subtitle}</p>
        {/if}
      </div>
      <button
        type="button"
        aria-label="Close"
        class="-mr-1 grid size-6 shrink-0 place-items-center rounded-md text-foreground-muted
          transition-colors hover:bg-muted hover:text-foreground"
        onclick={onclose}
      >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1.4" />
        </svg>
      </button>
    </header>

    <div class="min-h-0 flex-1 overflow-y-auto px-4 py-3">
      {@render children()}
    </div>

    {#if footer}
      <footer class="flex shrink-0 items-center gap-2 border-t border-line px-4 py-3">
        {@render footer()}
      </footer>
    {/if}
  </div>
</div>
