<script lang="ts">
  import { untrack } from "svelte";
  import { daemon } from "$lib/daemon.svelte";
  import { wallpaperApp, wallpaperDelay, withWallpaperDelay } from "$lib/wallpaper";
  import type { Rule } from "$lib/types";
  import Button from "$lib/ui/Button.svelte";
  import Dialog from "$lib/ui/Dialog.svelte";

  let { rule, onclose }: { rule: Rule; onclose: () => void } = $props();
  const initial = untrack(() => wallpaperDelay(rule) ?? 300);
  let unit = $state(initial % 60 === 0 ? 60 : 1);
  let amount = $state<number | undefined>(initial / (initial % 60 === 0 ? 60 : 1));
  const seconds = $derived((amount ?? 0) * unit);
  const valid = $derived(Number.isInteger(seconds) && seconds >= 1 && seconds <= 86400);
  const app = $derived(wallpaperApp(daemon.doctor?.capabilities ?? []));

  async function save() {
    if (!valid) return;
    const next = withWallpaperDelay(rule, seconds);
    if (await daemon.saveRules(daemon.rules.map((r) => r.name === rule.name ? next : r), `${app} settings saved`)) onclose();
  }

  async function remove() {
    if (await daemon.saveRules(daemon.rules.filter((r) => r.name !== rule.name), `${rule.name} deleted`)) onclose();
  }
</script>

<Dialog title={app} subtitle="Pause when you're away" {onclose}>
  <div class="flex flex-col gap-5 py-2">
    <p class="text-[13px] leading-relaxed text-foreground-muted">
      Pause your animated wallpaper after a period without keyboard or mouse input.
    </p>
    <div class="flex flex-col gap-2">
      <label for="wallpaper-delay" class="text-[13px] font-medium">Pause after</label>
      <div class="flex items-center gap-2">
        <input id="wallpaper-delay" class="field num w-24" type="number" min="1"
          max={86400 / unit} step="1" bind:value={amount} aria-invalid={!valid} />
        <select class="field w-32" aria-label="Delay unit" bind:value={unit}>
          <option value={60}>minutes</option>
          <option value={1}>seconds</option>
        </select>
        <span class="text-xs text-foreground-muted">without activity</span>
      </div>
      {#if !valid}
        <p class="text-xs text-danger">Choose a delay between 1 second and 24 hours.</p>
      {/if}
    </div>
    <div class="flex flex-col gap-1 border-t border-line pt-4">
      <p class="text-[13px] font-medium">Resume when you return</p>
      <p class="text-xs leading-relaxed text-foreground-muted">
        Keyboard or mouse activity restores playback within about a second.
        Cortisoff sends a play command, even if you had paused it manually.
      </p>
    </div>
    <p class="text-xs leading-relaxed text-foreground-muted">
      Fullscreen applications and audio playback keep your wallpaper running.
    </p>
  </div>
  {#snippet footer()}
    <Button variant="danger" onclick={remove} disabled={daemon.busy}>Delete</Button>
    <div class="ml-auto flex items-center gap-2">
      <Button variant="quiet" onclick={onclose}>Cancel</Button>
      <Button variant="primary" onclick={save} disabled={daemon.busy || !valid}>Save</Button>
    </div>
  {/snippet}
</Dialog>
