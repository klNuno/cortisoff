<script lang="ts">
  import { daemon } from "$lib/daemon.svelte";
  import {
    capabilityLabel,
    capabilityOf,
    describeAssign,
    describeCondition,
    describeTransition,
  } from "$lib/describe";
  import { kelvinHex } from "$lib/ranges";
  import type { CapabilityReport, ClaimReport, Rule, RuleReport, Value } from "$lib/types";
  import Switch from "$lib/ui/Switch.svelte";

  /**
   * One plugin, as a card.
   *
   * A plugin is a rule in the file, so this is the whole product on screen:
   * its name in full, when it fires, what it sets, whether it is
   * doing it right now, a switch, and one button for the thing you actually
   * came to click. The card grows with its content rather than cutting it.
   */
  let {
    rule,
    report,
    claims,
    capabilities,
    onedit,
  }: {
    rule: Rule;
    /** What the machine made of it. Absent for a beat after a save. */
    report: RuleReport | undefined;
    claims: ClaimReport[];
    capabilities: CapabilityReport[];
    onedit: () => void;
  } = $props();

  const fade = $derived(describeTransition(rule.transition));
  const state = $derived(report?.state);
  const holding = $derived(report?.holding ?? false);
  const missing = $derived(state?.state === "unsupported" ? state.missing : []);
  const unsupported = $derived(missing.length > 0);

  /** The capabilities this rule sets that this machine can actually write. */
  const writable = $derived(
    rule.set.filter((assign) =>
      capabilities.some((c) => c.id === assign.cap && c.settable && !c.missing_because),
    ),
  );

  /** True while the user holds one of them by hand, which outranks the rule. */
  const overridden = $derived(
    writable.length > 0 &&
      writable.every((assign) =>
        claims.some((c) => c.cap === assign.cap && c.owner === "manual"),
      ),
  );

  const status = $derived(
    unsupported
      ? `needs ${missing.join(", ")}, which this machine does not have`
      : !rule.enabled
        ? "off"
        : state?.state === "empty"
          ? "sets nothing"
          : holding
            ? "holding now"
            : "waiting",
  );

  const dot = $derived(
    unsupported
      ? "bg-danger"
      : !rule.enabled
        ? "bg-transparent ring-1 ring-foreground-subtle/40"
        : holding
          ? "bg-success"
          : "bg-foreground-subtle/50",
  );

  /**
   * A colour temperature is shown as the colour it is: the daemon says the
   * unit, and kelvin is the one unit that has a colour.
   */
  function swatch(cap: string, value: Value): string | null {
    const control = capabilityOf(cap, capabilities)?.control;
    if (control?.kind !== "range" || control.unit !== "K" || !("int" in value)) return null;
    return kelvinHex(value.int);
  }

  async function toggle(next: boolean) {
    const rules = daemon.rules.map((r) => (r.name === rule.name ? { ...r, enabled: next } : r));
    await daemon.saveRules(rules, `${rule.name} is ${next ? "on" : "off"}`);
  }

  /** Everything the rule sets, held by hand until released. */
  async function applyNow() {
    for (const assign of writable) {
      const ok = await daemon.apply(assign.cap, assign.value);
      if (!ok) return;
    }
  }

  async function release() {
    for (const assign of writable) {
      await daemon.clear(assign.cap);
    }
  }
</script>

<article
  class="card-in flex min-h-[196px] flex-col gap-3 rounded-card border bg-surface p-3.5
    transition-colors
    {holding && rule.enabled ? 'border-success/30' : 'border-line'}
    {rule.enabled ? '' : 'opacity-60'}"
>
  <header class="flex items-start gap-2.5">
    <span class="mt-[7px] size-2 shrink-0 rounded-full {dot}" title={status}></span>
    <h3 class="min-w-0 flex-1 text-sm font-semibold leading-snug break-words">
      {rule.name}
    </h3>
    <button
      type="button"
      aria-label="Settings for {rule.name}"
      title="Settings"
      class="-mr-1.5 -mt-1 grid size-7 shrink-0 place-items-center rounded-md
        text-foreground-subtle transition-colors hover:bg-muted hover:text-foreground"
      onclick={onedit}
    >
      <svg
        width="13"
        height="13"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        aria-hidden="true"
      >
        <circle cx="12" cy="12" r="3" />
        <path
          d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0
             0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65
             1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65
             1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0
             0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0
             0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2
             2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1
             1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"
        />
      </svg>
    </button>
  </header>

  <p class="text-xs leading-relaxed text-foreground-muted">
    {describeCondition(rule.when)}
  </p>

  <!-- What the rule sets, one line each, the value in the same face as the
       name and never cut: a fade's "6600 to 3200 K" wraps under its label. -->
  <ul class="flex flex-col gap-1.5 rounded-lg bg-(--well-bg) px-2.5 py-2">
    {#each rule.set as assign (assign.cap)}
      {@const colour = swatch(assign.cap, assign.value)}
      <li class="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 text-[13px]">
        <span class="min-w-0 text-foreground-subtle" title={assign.cap}>
          {capabilityLabel(assign.cap, capabilities)}
        </span>
        <span class="num ml-auto flex items-baseline gap-1.5 font-medium text-foreground">
          {#if colour}
            <span
              class="size-2 self-center rounded-full ring-1 ring-black/30"
              style:background={colour}
            ></span>
          {/if}
          {describeAssign(rule, assign.cap, assign.value, capabilityOf(assign.cap, capabilities)?.control)}
        </span>
      </li>
    {:else}
      <li class="text-xs text-foreground-subtle">sets nothing yet</li>
    {/each}
    {#if fade}
      <li class="text-xs text-foreground-subtle">{fade}</li>
    {/if}
  </ul>

  <footer class="mt-auto flex items-center gap-2 pt-0.5">
    <Switch
      checked={rule.enabled}
      disabled={daemon.busy || !daemon.connected || unsupported}
      label="Turn {rule.name} {rule.enabled ? 'off' : 'on'}"
      onchange={toggle}
    />
    <span class="min-w-0 flex-1 truncate text-xs text-foreground-muted" title={status}>
      {status}
    </span>

    {#if writable.length > 0}
      {#if overridden}
        <button
          type="button"
          class="h-7 shrink-0 rounded-md px-2.5 text-xs font-medium text-foreground-muted
            transition-colors hover:bg-muted hover:text-foreground disabled:opacity-40"
          disabled={daemon.busy}
          title="Give the capability back to the plugins"
          onclick={release}
        >
          Release
        </button>
      {:else}
        <button
          type="button"
          class="h-7 shrink-0 rounded-md bg-muted px-2.5 text-xs font-medium transition-colors
            hover:bg-elevated disabled:opacity-40"
          disabled={daemon.busy || !daemon.connected}
          title="Hold what this plugin sets, right now, whatever its condition says"
          onclick={applyNow}
        >
          Apply now
        </button>
      {/if}
    {/if}
  </footer>
</article>
