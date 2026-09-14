<script lang="ts">
  import { daemon } from "$lib/daemon.svelte";
  import { blankRule, templates, uniqueName, type Template } from "$lib/templates";
  import type { Rule } from "$lib/types";
  import Dialog from "$lib/ui/Dialog.svelte";

  /**
   * What the `+` card opens.
   *
   * Every entry is a working plugin rather than an empty form, because one
   * with nothing in it teaches nobody what a plugin is. Picking one adds it
   * switched off and opens its settings, so nothing changes a screen until the
   * person has looked at what they are about to turn on.
   */
  let { onclose, onadded }: { onclose: () => void; onadded: (name: string) => void } = $props();

  const available = $derived(daemon.doctor?.capabilities ?? []);

  function has(cap: string): boolean {
    return available.some((c) => c.id === cap && !c.missing_because);
  }

  function missingFor(template: Template): string[] {
    return template.needs.filter((cap) => !has(cap));
  }

  async function add(make: () => Rule) {
    const rule = make();
    rule.name = uniqueName(
      rule.name,
      daemon.rules.map((r) => r.name),
    );
    // Added switched off: the settings dialog opens on top of this, and a
    // plugin that starts changing the screen while you are still reading it
    // is not a dialog anyone trusts.
    rule.enabled = false;
    const ok = await daemon.saveRules([...daemon.rules, rule], `${rule.name} added, still off`);
    if (ok) onadded(rule.name);
  }
</script>

<Dialog title="Add a plugin" subtitle="Pick one to start from. It is added switched off." {onclose}>
  <ul class="flex flex-col gap-2">
    {#each templates as template (template.id)}
      {@const missing = missingFor(template)}
      <li>
        <button
          type="button"
          class="w-full rounded-card border border-line bg-transparent p-3 text-left
            transition-colors hover:border-elevated hover:bg-muted
            disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-line
            disabled:hover:bg-transparent"
          disabled={missing.length > 0 || daemon.busy}
          onclick={() => add(template.make)}
        >
          <span class="text-[13px] font-semibold">{template.title}</span>
          <span class="mt-1 block text-xs leading-relaxed text-foreground-muted">
            {template.summary}
          </span>
          {#if missing.length > 0}
            <span class="mt-1 block text-xs text-foreground-subtle">
              needs {missing.join(", ")}, which this machine does not have
            </span>
          {/if}
        </button>
      </li>
    {/each}

    <li>
      <button
        type="button"
        class="w-full rounded-card border border-dashed border-line p-3 text-left text-[13px]
          text-foreground-muted transition-colors hover:border-elevated hover:text-foreground
          disabled:cursor-not-allowed disabled:opacity-40"
        disabled={daemon.busy}
        onclick={() => add(blankRule)}
      >
        Start from nothing
      </button>
    </li>
  </ul>
</Dialog>
