<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    onclick,
    variant = "default",
    disabled = false,
    title,
    children,
  }: {
    onclick?: () => void;
    variant?: "default" | "primary" | "quiet" | "danger";
    disabled?: boolean;
    title?: string;
    children: Snippet;
  } = $props();

  const base =
    "inline-flex h-7.5 items-center rounded-md px-3 text-[13px] font-medium " +
    "transition-colors disabled:cursor-not-allowed disabled:opacity-40";

  const variants = {
    default: "bg-muted text-foreground hover:bg-elevated",
    // Primary actions use the foreground colour over a solid background.
    primary: "bg-foreground text-[var(--bg-solid)] hover:bg-foreground/85",
    quiet: "text-foreground-muted hover:bg-muted hover:text-foreground",
    danger: "text-danger hover:bg-danger hover:text-foreground",
  };
</script>

<button type="button" class="{base} {variants[variant]}" {onclick} {disabled} {title}>
  {@render children()}
</button>
