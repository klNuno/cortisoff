<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, untrack } from "svelte";

  import {
    flagLabels,
    fromClauses,
    isEmpty,
    newClause,
    toClauses,
    type Clause,
    type Flag,
  } from "$lib/clauses";
  import { daemon } from "$lib/daemon.svelte";
  import { capabilityLabel, capabilityOf, describeDuration, valueText } from "$lib/describe";
  import { isSwitch, numberOf, rangeFor } from "$lib/ranges";
  import type { Assign, Rule, Transition, Value } from "$lib/types";
  import Button from "$lib/ui/Button.svelte";
  import Dialog from "$lib/ui/Dialog.svelte";
  import Slider from "$lib/ui/Slider.svelte";
  import Switch from "$lib/ui/Switch.svelte";

  /**
   * Everything about one plugin that is not its switch.
   *
   * It edits a copy and saves the whole set, so closing without saving costs
   * nothing and a refused save leaves the dialog open on top of the reason.
   * A number with a known range is a slider; everything else is typed the way
   * the rule file spells it, and the daemon reads it back.
   */
  let { rule, onclose }: { rule: Rule; onclose: () => void } = $props();

  // Deliberately the initial value and nothing after it: this is a working
  // copy. The dialog is keyed on the rule's name, so picking a different rule
  // builds a different dialog rather than overwriting what someone is typing.
  let name = $state(untrack(() => rule.name));
  let priority = $state(untrack(() => rule.priority));
  let clauses = $state<Clause[] | null>(untrack(() => toClauses(rule.when)));
  /**
   * Values are edited as text, the way the rule file spells them, and a
   * slider writes its number into that text. `from` is where the value fades
   * from, empty for one that switches.
   */
  let sets = $state(
    untrack(() =>
      rule.set.map((a) => {
        const start = rule.transition?.from.find((f) => f.cap === a.cap);
        return { cap: a.cap, text: valueText(a.value), from: start ? valueText(start.value) : "" };
      }),
    ),
  );

  let fading = $state(untrack(() => Boolean(rule.transition)));
  let minutes = $state(untrack(() => Math.max(1, Math.round((rule.transition?.seconds ?? 60) / 60))));
  let offset = $state(untrack(() => rule.transition?.offset ?? 0));
  /**
   * A fade runs across a time window. A condition written by hand is given
   * the benefit of the doubt, and the daemon refuses the save if it has none.
   */
  const hasWindow = $derived(clauses === null || clauses.some((c) => c.kind === "between"));

  const capabilities = $derived(daemon.doctor?.capabilities ?? []);
  const settable = $derived(capabilities.filter((c) => c.settable && !c.missing_because));
  const bands = [
    { value: -100, label: "Background", hint: "Loses to every other plugin. A default." },
    { value: 0, label: "Normal", hint: "Where a schedule sits." },
    { value: 100, label: "Focus", hint: "Beats a schedule, for a game or a film." },
  ];
  const band = $derived(bands.find((b) => b.value === priority));

  const offsetHint = $derived(
    offset === 0
      ? "Both fades happen inside the window."
      : offset === 100
        ? "Both fades happen outside the window."
        : offset === 50
          ? "Each fade sits on its edge of the window."
          : `${offset}% of each fade happens outside the window.`,
  );

  function addClause() {
    clauses = [...(clauses ?? []), newClause("app")];
  }

  function replace(at: number, clause: Clause) {
    if (!clauses) return;
    clauses = clauses.map((c, i) => (i === at ? clause : c));
  }

  function removeClause(at: number) {
    if (!clauses) return;
    clauses = clauses.filter((_, i) => i !== at);
  }

  /** Kind changes reset the row: an app name means nothing to a time window. */
  function changeKind(at: number, kind: Clause["kind"]) {
    if (!clauses) return;
    const clause = newClause(kind);
    clause.not = clauses[at].not;
    replace(at, clause);
  }

  function addSet() {
    const unused = settable.find((c) => !sets.some((s) => s.cap === c.id));
    const cap = unused?.id ?? "";
    const range = rangeFor(unused?.control);
    sets = [...sets, { cap, text: range ? String(range.fallback) : "", from: "" }];
  }

  /** How the daemon says a capability is edited, or null for one it lacks. */
  function controlOf(cap: string) {
    return capabilityOf(cap, capabilities)?.control ?? null;
  }

  /**
   * A slider under the pointer shows its value on the real screen, and
   * letting go fades back to whatever held the capability before. Sent at
   * most every 40 ms: the fade back runs at that pace too, and a pipe
   * request per pixel would queue behind itself.
   */
  const PREVIEW_EVERY_MS = 40;
  let previewing = $state<string | null>(null);
  let previewTimer: ReturnType<typeof setTimeout> | null = null;
  let previewNext: { cap: string; value: number } | null = null;

  function previewable(cap: string): boolean {
    return daemon.connected && settable.some((c) => c.id === cap);
  }

  function preview(cap: string, value: number) {
    if (!previewable(cap)) return;
    previewing = cap;
    previewNext = { cap, value };
    if (previewTimer !== null) return;
    previewTimer = setTimeout(() => {
      previewTimer = null;
      const next = previewNext;
      previewNext = null;
      if (next && previewing === next.cap) void daemon.preview(next.cap, { int: next.value });
    }, PREVIEW_EVERY_MS);
  }

  function previewEnd() {
    const cap = previewing;
    if (!cap) return;
    previewing = null;
    if (previewTimer !== null) {
      clearTimeout(previewTimer);
      previewTimer = null;
    }
    const last = previewNext;
    previewNext = null;
    // The last dragged value lands before the fade back starts from it.
    const send = last ? daemon.preview(last.cap, { int: last.value }) : Promise.resolve();
    void send.then(() => daemon.previewEnd(cap));
  }

  // Closing the dialog mid-drag must not leave the preview on the screen.
  onDestroy(previewEnd);

  async function save() {
    const trimmed = name.trim();
    if (!trimmed) return;

    // The daemon parses values, so the dialog never has to know what `@240`
    // means. Every row at once, on save only.
    const rows = sets.filter((row) => row.cap.trim() && row.text.trim());
    const parsed = await Promise.all(
      rows.map(async (row) => {
        const [to, from] = await Promise.all([
          invoke<Value>("read_value", { text: row.text }),
          fading && row.from.trim()
            ? invoke<Value>("read_value", { text: row.from })
            : Promise.resolve(null),
        ]);
        return { cap: row.cap.trim(), to, from };
      }),
    );
    const set: Assign[] = parsed.map(({ cap, to }) => ({ cap, value: to }));
    const from: Assign[] = parsed.flatMap(({ cap, from }) => (from ? [{ cap, value: from }] : []));

    let transition: Transition | null = null;
    if (fading && hasWindow) {
      // Kept to the second when the minutes were not touched, so a `1m30s`
      // written by hand does not become `2m` by opening this dialog.
      const before = rule.transition;
      const seconds =
        before && Math.max(1, Math.round(before.seconds / 60)) === minutes
          ? before.seconds
          : Math.round(minutes) * 60;
      transition = { seconds, offset: Math.min(100, Math.max(0, Math.round(offset))), from };
    }

    const next: Rule = {
      ...rule,
      name: trimmed,
      priority,
      when: clauses ? fromClauses(clauses.filter((c) => !isEmpty(c))) : rule.when,
      set,
      transition,
    };
    const rules = daemon.rules.map((r) => (r.name === rule.name ? next : r));
    if (await daemon.saveRules(rules, `${trimmed} saved`)) onclose();
  }

  async function remove() {
    const rules = daemon.rules.filter((r) => r.name !== rule.name);
    if (await daemon.saveRules(rules, `${rule.name} deleted`)) onclose();
  }
</script>

{#snippet cross()}
  <svg width="9" height="9" viewBox="0 0 10 10" aria-hidden="true">
    <path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1.4" />
  </svg>
{/snippet}

<Dialog title={rule.name} subtitle="Plugin settings" {onclose}>
  <div class="flex flex-col gap-3">
    <section class="well flex flex-col gap-3.5 p-3.5">
      <label class="flex flex-col gap-1.5">
        <span class="section-label">Name</span>
        <input class="field" bind:value={name} />
      </label>

      <div class="flex flex-col gap-1.5">
        <span class="section-label">Priority</span>
        <div class="flex flex-wrap items-center gap-3">
          <div
            class="inline-flex shrink-0 rounded-md border border-edge bg-field p-0.5"
            role="radiogroup"
            aria-label="Priority"
          >
            {#each bands as b (b.value)}
              <button
                type="button"
                role="radio"
                aria-checked={priority === b.value}
                class="h-7 rounded px-3 text-xs font-medium transition-colors
                  {priority === b.value
                  ? 'bg-elevated text-foreground'
                  : 'text-foreground-muted hover:text-foreground'}"
                onclick={() => (priority = b.value)}
              >
                {b.label}
              </button>
            {/each}
          </div>
          <p class="text-xs text-foreground-muted">
            {band ? band.hint : `${priority}, set by hand in the file.`}
          </p>
        </div>
      </div>
    </section>

    <section class="well flex flex-col gap-2.5 p-3.5">
      <header class="flex items-center justify-between">
        <h3 class="section-label">Fires while</h3>
        {#if clauses !== null}
          <button
            type="button"
            class="rounded-md px-2 py-1 text-xs font-medium text-foreground-muted transition-colors
              hover:bg-muted hover:text-foreground"
            onclick={addClause}
          >
            + condition
          </button>
        {/if}
      </header>

      {#if clauses === null}
        <p class="text-[13px] leading-relaxed text-foreground-muted">
          This condition was written by hand and does not fit these rows. It is left exactly as
          it is. Edit it in the file.
        </p>
      {:else}
        {#each clauses as clause, i (i)}
          <div class="flex flex-wrap items-center gap-1.5">
            <button
              type="button"
              class="h-7.5 w-12 shrink-0 rounded-md border text-xs font-medium transition-colors
                {clause.not
                ? 'border-danger/50 bg-danger/10 text-danger'
                : 'border-edge bg-field text-foreground-muted hover:text-foreground'}"
              title={clause.not ? "This row has to be false" : "This row has to be true"}
              onclick={() => replace(i, { ...clause, not: !clause.not })}
            >
              {clause.not ? "not" : "is"}
            </button>

            <select
              class="field shrink-0"
              value={clause.kind}
              onchange={(e) => changeKind(i, e.currentTarget.value as Clause["kind"])}
            >
              <option value="app">application</option>
              <option value="title">window title</option>
              <option value="flag">machine state</option>
              <option value="between">time of day</option>
            </select>

            {#if clause.kind === "app"}
              <input
                class="field flex-1"
                placeholder="cs2.exe, vlc.exe"
                value={clause.apps.join(", ")}
                oninput={(e) =>
                  replace(i, { ...clause, apps: e.currentTarget.value.split(",").map((a) => a.trim()) })}
              />
            {:else if clause.kind === "title"}
              <input
                class="field flex-1"
                placeholder="youtube"
                value={clause.text}
                oninput={(e) => replace(i, { ...clause, text: e.currentTarget.value })}
              />
            {:else if clause.kind === "flag"}
              <select
                class="field flex-1"
                value={clause.flag}
                onchange={(e) => replace(i, { ...clause, flag: e.currentTarget.value as Flag })}
              >
                {#each Object.entries(flagLabels) as [flag, label] (flag)}
                  <option value={flag}>{label}</option>
                {/each}
              </select>
            {:else}
              <input
                type="time"
                class="field shrink-0"
                value={clause.start}
                oninput={(e) => replace(i, { ...clause, start: e.currentTarget.value })}
              />
              <span class="text-xs text-foreground-muted">to</span>
              <input
                type="time"
                class="field shrink-0"
                value={clause.end}
                oninput={(e) => replace(i, { ...clause, end: e.currentTarget.value })}
              />
            {/if}

            <button
              type="button"
              aria-label="Remove this condition"
              class="grid size-7 shrink-0 place-items-center rounded-md text-foreground-subtle
                transition-colors hover:bg-muted hover:text-foreground"
              onclick={() => removeClause(i)}
            >
              {@render cross()}
            </button>
          </div>
        {/each}

        {#if clauses.length === 0}
          <p class="text-xs text-foreground-muted">
            No condition, so this plugin holds all the time and everything above it wins.
          </p>
        {/if}
      {/if}
    </section>

    <section class="well flex flex-col gap-2.5 p-3.5">
      <header class="flex items-center justify-between">
        <h3 class="section-label">Sets</h3>
        <button
          type="button"
          class="rounded-md px-2 py-1 text-xs font-medium text-foreground-muted transition-colors
            hover:bg-muted hover:text-foreground"
          onclick={addSet}
        >
          + capability
        </button>
      </header>

      {#each sets as row, i (i)}
        {@const control = controlOf(row.cap)}
        {@const range = rangeFor(control)}
        <div class="flex flex-col gap-2.5 rounded-lg border border-line/70 bg-surface/40 p-2.5">
          <div class="flex items-center gap-1.5">
            <select class="field flex-1" bind:value={row.cap}>
              {#each settable as cap (cap.id)}
                <option value={cap.id}>{cap.label}</option>
              {/each}
              {#if !settable.some((c) => c.id === row.cap)}
                <option value={row.cap}>{row.cap || "pick a capability"}</option>
              {/if}
            </select>
            {#if isSwitch(control)}
              <span class="text-xs text-foreground-muted">
                {row.text === "false" ? "off" : "on"}
              </span>
              <Switch
                checked={row.text !== "false"}
                label={capabilityLabel(row.cap, capabilities)}
                onchange={(on) => (row.text = on ? "true" : "false")}
              />
            {:else if !range}
              <input
                class="field w-36 shrink-0"
                placeholder={control?.kind === "text" ? control.hint : ""}
                bind:value={row.text}
              />
            {/if}
            <button
              type="button"
              aria-label="Remove this capability"
              class="grid size-7 shrink-0 place-items-center rounded-md text-foreground-subtle
                transition-colors hover:bg-muted hover:text-foreground"
              onclick={() => (sets = sets.filter((_, at) => at !== i))}
            >
              {@render cross()}
            </button>
          </div>

          {#if range}
            <Slider
              value={numberOf(row.text, range)}
              min={range.min}
              max={range.max}
              step={range.step}
              label={fading && hasWindow ? "ends at" : "value"}
              text={range.text(numberOf(row.text, range))}
              track={range.track}
              onchange={(v) => {
                row.text = String(v);
                preview(row.cap, v);
              }}
              onrelease={previewEnd}
            />
            {#if previewable(row.cap)}
              <p class="-mt-1 text-xs text-foreground-muted">
                Shown on the screen while you drag. Letting go fades back.
              </p>
            {/if}
          {/if}
        </div>
      {/each}

      {#if sets.length === 0}
        <p class="text-xs text-foreground-muted">
          Nothing set, so this plugin only shows in the logs.
        </p>
      {/if}
    </section>

    {#if sets.some((row) => rangeFor(controlOf(row.cap)) !== null)}
    <section class="well flex flex-col gap-3 p-3.5">
      <header class="flex items-center justify-between gap-3">
        <div class="flex flex-col gap-0.5">
          <h3 class="section-label">Fade</h3>
          <p class="text-xs text-foreground-muted">
            {#if !hasWindow}
              A fade runs across a time of day. Add one to the conditions to use it.
            {:else}
              Eases in and out along the window instead of switching at its edges.
            {/if}
          </p>
        </div>
        <Switch
          checked={fading && hasWindow}
          disabled={!hasWindow}
          label="Fade instead of switching"
          onchange={(next) => (fading = next)}
        />
      </header>

      {#if fading && hasWindow}
        <div class="grid gap-3 sm:grid-cols-2">
          <Slider
            value={minutes}
            min={1}
            max={180}
            label="over"
            text={describeDuration(minutes * 60)}
            onchange={(v) => (minutes = v)}
          />
          <Slider
            value={offset}
            min={0}
            max={100}
            step={5}
            label="offset"
            text={`${offset}%`}
            onchange={(v) => (offset = v)}
          />
        </div>
        <p class="-mt-1 text-xs text-foreground-muted">{offsetHint}</p>

        <!--
          Where each value fades from. Nothing named means the value the screen
          shows when no plugin holds it, which the daemon reads from the
          actuator: 6600 K for colour, the driver's current level for vibrance.
          A number pinned here starts the fade there instead.
        -->
        {#each sets as row, i (i)}
          {@const control = controlOf(row.cap)}
          {@const range = rangeFor(control)}
          {#if row.cap && range}
            {#if row.from.trim()}
              <div class="flex items-end gap-2">
                <div class="min-w-0 flex-1">
                  <Slider
                    value={numberOf(row.from, range)}
                    min={range.min}
                    max={range.max}
                    step={range.step}
                    label="{capabilityLabel(row.cap, capabilities)} starts from"
                    text={range.text(numberOf(row.from, range))}
                    track={range.track}
                    onchange={(v) => {
                      row.from = String(v);
                      preview(row.cap, v);
                    }}
                    onrelease={previewEnd}
                  />
                </div>
                <Button variant="quiet" title="Start from whatever the screen shows" onclick={() => (row.from = "")}>
                  current
                </Button>
              </div>
            {:else}
              <div class="flex items-center gap-2">
                <span class="min-w-0 flex-1 truncate text-[13px] text-foreground-muted">
                  {capabilityLabel(row.cap, capabilities)} starts from
                </span>
                <span class="text-[13px] text-foreground">the current value</span>
                <Button variant="quiet" title="Start the fade from a value of your own" onclick={() => (row.from = String(range.fallback))}>
                  pick one
                </Button>
              </div>
            {/if}
          {:else if row.cap && !isSwitch(control)}
            <label class="flex items-center gap-2">
              <span class="min-w-0 flex-1 truncate text-[13px] text-foreground-muted">
                {capabilityLabel(row.cap, capabilities)} starts from
              </span>
              <input class="field w-32 shrink-0" placeholder="current" bind:value={row.from} />
            </label>
          {/if}
        {/each}
      {/if}
    </section>
    {/if}
  </div>

  {#snippet footer()}
    <Button variant="danger" onclick={remove} disabled={daemon.busy}>Delete</Button>
    <div class="ml-auto flex items-center gap-2">
      <Button variant="quiet" onclick={onclose}>Cancel</Button>
      <Button variant="primary" onclick={save} disabled={daemon.busy || !name.trim()}>Save</Button>
    </div>
  {/snippet}
</Dialog>
