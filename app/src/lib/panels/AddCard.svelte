<script lang="ts">
  /**
   * The card that is not a plugin.
   *
   * Dashed rather than filled, so the grid reads as "these things exist, and
   * there is room for another" instead of one card being broken.
   */
  import { entrance } from "$lib/opening";

  let { onclick, disabled = false }: { onclick: () => void; disabled?: boolean } = $props();
  const enter = entrance("card-in");
</script>

<button
  type="button"
  {disabled}
  {onclick}
  title={disabled ? "Start cortisoffd to add a plugin" : "Add a plugin"}
  class="{enter} flex min-h-32 flex-col items-center justify-center gap-2 rounded-card border
    border-dashed border-line text-foreground-subtle transition-colors
    hover:border-elevated hover:bg-surface hover:text-foreground-muted
    disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-line
    disabled:hover:bg-transparent"
>
  <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
    <path d="M12 5v14M5 12h14" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
  </svg>
  <span class="text-[13px]">Add a plugin</span>
</button>
