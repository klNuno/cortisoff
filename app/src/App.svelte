<script lang="ts">
  import { daemon } from "$lib/daemon.svelte";
  import AddCard from "$lib/panels/AddCard.svelte";
  import RuleCard from "$lib/panels/RuleCard.svelte";
  import RuleDialog from "$lib/panels/RuleDialog.svelte";
  import SettingsDialog from "$lib/panels/SettingsDialog.svelte";
  import TemplatePicker from "$lib/panels/TemplatePicker.svelte";
  import { exeName } from "$lib/types";
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
  const reports = $derived(daemon.doctor?.rules ?? []);
  const editingRule = $derived(daemon.rules.find((r) => r.name === editing) ?? null);

  /** The one line above the grid: what the machine is doing right now. */
  const seeing = $derived.by(() => {
    if (!world) return "";
    const parts: string[] = [];
    parts.push(world.app ? exeName(world.app.exe) : "nothing focused");
    if (world.fullscreen) parts.push("exclusive fullscreen");
    else if (world.maximized) parts.push("filling the screen");
    if (world.idle) parts.push("you are away");
    if (world.locked) parts.push("locked");
    if (world.on_battery) parts.push("on battery");
    if (world.audio_active) parts.push("audio playing");
    if (world.minute_of_day !== null) {
      const h = String(Math.floor(world.minute_of_day / 60)).padStart(2, "0");
      const m = String(world.minute_of_day % 60).padStart(2, "0");
      parts.push(`${h}:${m}`);
    }
    return parts.join("  ·  ");
  });
</script>

<!-- The shell is the only thing that paints a ground, and it paints it
     translucent so the OS blur shows through. -->
<div class="relative flex h-full flex-col overflow-hidden bg-background">
  <Titlebar onsettings={() => (panel = "settings")} />

  <main class="page-in flex min-h-0 flex-1 flex-col gap-3 p-3">
    <p class="shrink-0 truncate text-xs text-foreground-muted" title={world?.app?.title ?? ""}>
      {daemon.connected ? seeing : "start cortisoffd, and this window connects on its own"}
    </p>

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
      <RuleDialog rule={editingRule} onclose={() => (editing = null)} />
    {/key}
  {/if}

  <Toasts />
</div>
