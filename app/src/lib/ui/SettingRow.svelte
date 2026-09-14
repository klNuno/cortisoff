<script lang="ts">
  import type { Snippet } from "svelte";

  /**
   * One setting: a name, a line under it, a control at the right.
   *
   * All settings tabs share this row's spacing and text sizes.
   */
  let {
    label,
    description,
    children,
  }: {
    label: string;
    description?: string;
    children: Snippet;
  } = $props();
</script>

<div class="flex items-center gap-3 px-2.5 py-2.5">
  <div class="min-w-0 flex-1">
    <div class="text-[13px] font-medium">{label}</div>
    {#if description}
      <div class="mt-0.5 text-xs leading-snug text-foreground-muted">{description}</div>
    {/if}
  </div>
  <div class="flex shrink-0 items-center">
    {@render children()}
  </div>
</div>
