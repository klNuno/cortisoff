<script lang="ts">
  import { daemon } from "$lib/daemon.svelte";
  import AddCard from "$lib/panels/AddCard.svelte";
  import RuleCard from "$lib/panels/RuleCard.svelte";
  import WallpaperDialog from "$lib/panels/WallpaperDialog.svelte";
  import { wallpaperDelay } from "$lib/wallpaper";
  import RuleDialog from "$lib/panels/RuleDialog.svelte";
  import SettingsDialog from "$lib/panels/SettingsDialog.svelte";
  import TemplatePicker from "$lib/panels/TemplatePicker.svelte";
  import { entrance } from "$lib/opening";
  import { exeName } from "$lib/types";
  import Button from "$lib/ui/Button.svelte";
  import Toasts from "$lib/ui/Toasts.svelte";
  import Titlebar from "$lib/ui/Titlebar.svelte";

  /**
   * One screen: what cortisoff sees, and a card per plugin.
   *
   * Everything else is a dialog, because everything else is something you look
   * at once. The grid is the product.
   */
  type Panel = "none" | "settings" | "add";
  let panel = $state<Panel>("none");
  /** The plugin whose settings are open, by name. Names are the identity. */
  let editing = $state<string | null>(null);

  const world = $derived(daemon.state?.world ?? null);
  const claims = $derived(daemon.state?.claims ?? []);
  const capabilities = $derived(daemon.doctor?.capabilities ?? []);
  // From the state report, which every event refreshes, rather than the
  // doctor's, which is only asked for when the rule set changes.
  const reports = $derived(daemon.state?.rules ?? []);
  const editingRule = $derived(daemon.rules.find((r) => r.name === editing) ?? null);

  /** The one line above the grid: what the machine is doing right now. */
  const seeing = $derived.by(() => {
    if (!world) return "";
    const parts: string[] = [];
    // Null also when no plugin reads the focus, and then the daemon does not
    // watch it: "nothing focused" would be a guess.
    if (world.app) parts.push(exeName(world.app.exe));
    if (world.fullscreen) parts.push("exclusive fullscreen");
    else if (world.maximized) parts.push("filling the screen");
    if (world.idle) parts.push("you are away");
    if (world.locked) parts.push("locked");
    if (world.on_battery) parts.push("on battery");
    if (world.audio_active) parts.push("audio playing");
    // No clock: the daemon's is the last time it needed, which can be hours
    // old, and the person reading this has one of their own.
    return parts.join("  ·  ");
  });
</script>

<!-- The shell is the only thing that paints a ground, and it paints it
     translucent so the OS blur shows through. -->
<div class="relative flex h-full flex-col overflow-hidden bg-background">
  <Titlebar onsettings={() => (panel = "settings")} />

  <main class="flex min-h-0 flex-1 flex-col gap-3 p-3">
    <!-- A no-break space when there is nothing to say, so the grid does not
         jump by a line when there is. -->
    <p class="shrink-0 truncate text-xs text-foreground-muted" title={world?.app?.title ?? ""}>
      {(daemon.connected && seeing) || "\u00a0"}
    </p>

    {#if daemon.phase === "down"}
      <!-- The daemon is what does everything, so without it the grid would
           be cards nobody can act on. Say so once, with the one fix. -->
      <div
        class="{entrance('page-in')} flex flex-1 flex-col items-center justify-center gap-3 pb-10
          text-center"
      >
        <p class="text-sm font-semibold">The daemon is not running</p>
        <p class="max-w-sm text-[13px] leading-relaxed text-foreground-muted">
          {daemon.reason && daemon.reason !== "the daemon is not running"
            ? daemon.reason
            : "Nothing is being filtered or paused. This window connects on its own as soon as it is back."}
        </p>
        <Button variant="primary" onclick={() => daemon.startDaemon()} disabled={daemon.busy}>
          Start it
        </Button>
      </div>
    {:else if daemon.phase === "connecting"}
      <div class="flex flex-1 items-center justify-center pb-10">
        <span class="text-[13px] text-foreground-subtle">connecting</span>
      </div>
    {:else}
    <div class="min-h-0 flex-1 overflow-y-auto">
      <div class="grid grid-cols-[repeat(auto-fill,minmax(230px,1fr))] gap-3">
        {#each daemon.rules as rule (rule.name)}
          <RuleCard
            {rule}
            report={reports.find((r) => r.name === rule.name)}
            {claims}
            {capabilities}
            onedit={() => (editing = rule.name)}
          />
        {/each}
        <AddCard onclick={() => (panel = "add")} disabled={!daemon.connected || daemon.busy} />
      </div>

      {#if daemon.connected && daemon.rules.length === 0}
        <p class="mt-6 text-center text-[13px] text-foreground-muted">
          No plugins yet. Add one from the card above.
        </p>
      {/if}
    </div>
    {/if}
  </main>

  {#if panel === "settings"}
    <SettingsDialog onclose={() => (panel = "none")} />
  {:else if panel === "add"}
    <TemplatePicker
      onclose={() => (panel = "none")}
      onadded={(name) => {
        panel = "none";
        editing = name;
      }}
    />
  {/if}

  <!-- Keyed on the name so switching plugins builds a fresh dialog rather
       than leaving one plugin's fields over another's. -->
  {#if editingRule}
    {#key editingRule.name}
      {#if wallpaperDelay(editingRule) !== null}
        <WallpaperDialog rule={editingRule} onclose={() => (editing = null)} />
      {:else}
        <RuleDialog rule={editingRule} onclose={() => (editing = null)} />
      {/if}
    {/key}
  {/if}

  <Toasts />
</div>
