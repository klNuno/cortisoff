<script lang="ts">
  /**
   * On or off, and the one control on a card that has to be unmistakable.
   *
   * A button rather than a checkbox: it is never submitted, and the label sits
   * outside so the whole row can be the hit target without a wrapping label
   * swallowing the click.
   */
  let {
    checked,
    disabled = false,
    label,
    onchange,
  }: {
    checked: boolean;
    disabled?: boolean;
    /** Read by a screen reader, since the control itself has no text. */
    label: string;
    onchange: (next: boolean) => void;
  } = $props();
</script>

<button
  type="button"
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border transition-colors
    disabled:cursor-not-allowed disabled:opacity-40
    {checked ? 'border-success/60 bg-success/25' : 'border-line bg-muted'}"
  onclick={() => onchange(!checked)}
>
  <span
    class="size-3.5 rounded-full transition-transform
      {checked ? 'translate-x-[18px] bg-success' : 'translate-x-[3px] bg-foreground-subtle'}"
  ></span>
</button>
