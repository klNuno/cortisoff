<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { untrack } from "svelte";

  import { daemon } from "$lib/daemon.svelte";
  import { settings, type Scheme } from "$lib/settings.svelte";
  import { toast } from "$lib/toasts.svelte";
  import { updater } from "$lib/updater.svelte";
  import Button from "$lib/ui/Button.svelte";
  import SettingRow from "$lib/ui/SettingRow.svelte";
  import Switch from "$lib/ui/Switch.svelte";

  /**
   * Everything that is not a plugin.
   *
   * One dialog, a sidebar of three tabs: how the window looks, what this
   * machine can do, and the log.
   */
  type Tab = "general" | "machine" | "logs";

  let { tab = "general", onclose }: { tab?: Tab; onclose: () => void } = $props();

  // The tab it opens on, once: switching tabs afterwards is this dialog's own.
  let current = $state<Tab>(untrack(() => tab));

  const tabs: { id: Tab; label: string }[] = [
    { id: "general", label: "General" },
    { id: "machine", label: "This machine" },
    { id: "logs", label: "Logs" },
  ];

  const schemes: { id: Scheme; label: string }[] = [
    { id: "system", label: "System" },
    { id: "light", label: "Light" },
    { id: "dark", label: "Dark" },
  ];

  const capabilities = $derived(daemon.doctor?.capabilities ?? []);
  const present = $derived(capabilities.filter((c) => !c.missing_because));
  const warnings = $derived(daemon.doctor?.warnings ?? []);
  const dirty = $derived(daemon.doctor?.journal_dirty ?? []);

  /** Null until Task Scheduler has answered, or when it cannot. */
  let autostart = $state<boolean | null>(null);

  // Once, when the dialog opens. It reads nothing reactive, so it is not an
  // effect.
  invoke<boolean>("autostart")
    .then((on) => (autostart = on))
    .catch(() => (autostart = null));

  async function setAutostart(on: boolean) {
    try {
      await invoke("set_autostart", { on });
      autostart = on;
      toast(on ? "cortisoff starts when you log on" : "cortisoff no longer starts when you log on");
    } catch (e) {
      toast(typeof e === "string" ? e : String(e), "error");
    }
  }

  let logBox = $state<HTMLDivElement | null>(null);
  let pinned = $state(true);

  /** Follow the tail unless the reader has scrolled up to look at something. */
  $effect(() => {
    void daemon.trace.length;
    if (current === "logs" && pinned && logBox) logBox.scrollTop = logBox.scrollHeight;
  });

  function onLogScroll() {
    if (!logBox) return;
    pinned = logBox.scrollHeight - logBox.scrollTop - logBox.clientHeight < 24;
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window {onkeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events (Escape is handled above,
     and the backdrop is a convenience rather than the only way out) -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="page-in absolute inset-0 z-20 flex items-center justify-center bg-black/40 p-4"
  onclick={onclose}
>
  <div
    class="card-in flex h-[min(540px,100%)] w-full max-w-2xl overflow-hidden rounded-xl border
      border-line bg-surface shadow-(--shadow-dialog)"
    role="dialog"
    aria-modal="true"
    aria-label="Settings"
    tabindex="-1"
    onclick={(event) => event.stopPropagation()}
  >
    <aside class="flex w-[168px] shrink-0 flex-col gap-0.5 border-r border-line p-2.5">
      <div class="px-2 pb-2 pt-1 text-xs font-semibold text-foreground-subtle">Settings</div>
      {#each tabs as t (t.id)}
        <button
          type="button"
          data-tab={t.id}
          aria-current={current === t.id ? "page" : undefined}
          class="flex h-8 items-center gap-2 rounded-lg px-2.5 text-left text-[13px] font-semibold
            transition-colors
            {current === t.id
            ? 'bg-(--nav-active) text-foreground'
            : 'text-foreground-muted hover:bg-(--nav-hover) hover:text-foreground'}"
          onclick={() => (current = t.id)}
        >
          {#if t.id === "general"}
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              aria-hidden="true"
            >
              <path d="M4 6h8M18 6h2M4 12h2M10 12h10M4 18h10M18 18h2" />
              <circle cx="15" cy="6" r="2" />
              <circle cx="7" cy="12" r="2" />
              <circle cx="15" cy="18" r="2" />
            </svg>
          {:else if t.id === "machine"}
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              aria-hidden="true"
            >
              <rect x="2" y="4" width="20" height="14" rx="2" />
              <path d="M8 21h8M12 18v3" />
            </svg>
          {:else}
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              aria-hidden="true"
            >
              <path d="M4 6h16M4 12h16M4 18h10" />
            </svg>
          {/if}
          <span class="min-w-0 flex-1 truncate">{t.label}</span>
          {#if t.id === "general" && updater.stage === "ready"}
            <span class="size-1.5 rounded-full bg-success"></span>
          {/if}
        </button>
      {/each}
    </aside>

    <section class="flex min-w-0 flex-1 flex-col">
      <header class="flex h-11 shrink-0 items-center gap-3 border-b border-line pl-4 pr-2">
        <h2 class="min-w-0 flex-1 truncate text-sm font-semibold tracking-tight">
          {tabs.find((t) => t.id === current)?.label}
        </h2>
        {#if current === "machine"}
          <span class="text-xs text-foreground-muted">
            {daemon.doctor ? `cortisoff ${daemon.doctor.version}` : "not connected"}
          </span>
        {/if}
        <button
          type="button"
          aria-label="Close"
          class="grid size-7 shrink-0 place-items-center rounded-md text-foreground-muted
            transition-colors hover:bg-muted hover:text-foreground"
          onclick={onclose}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1.4" />
          </svg>
        </button>
      </header>

      {#if current === "general"}
        <div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
          {#if updater.message}
            <div class="flex items-center gap-3 rounded-card border border-line bg-muted px-3 py-2">
              <p class="min-w-0 flex-1 text-[13px]">{updater.message}</p>
              <Button
                onclick={() => updater.install()}
                disabled={updater.stage !== "ready"}
                title="Installs and restarts the window. The daemon keeps running."
              >
                Restart to update
              </Button>
            </div>
          {/if}

          <section class="well flex flex-col divide-y divide-line/70 px-1 py-1">
            <h3 class="px-2.5 pb-1 pt-2 text-sm font-semibold">Appearance</h3>
            <SettingRow label="Theme" description="System follows the OS.">
              <div
                class="flex rounded-md border border-edge bg-field p-0.5"
                role="radiogroup"
                aria-label="Theme"
              >
                {#each schemes as s (s.id)}
                  <button
                    type="button"
                    role="radio"
                    aria-checked={settings.scheme === s.id}
                    class="h-7 rounded-[5px] px-3 text-xs font-medium transition-colors
                      {settings.scheme === s.id
                      ? 'bg-muted text-foreground'
                      : 'text-foreground-muted hover:text-foreground'}"
                    onclick={() => settings.setScheme(s.id)}
                  >
                    {s.label}
                  </button>
                {/each}
              </div>
            </SettingRow>
            <SettingRow
              label="Translucent window"
              description="Acrylic blur through the window, in either theme."
            >
              <Switch
                checked={settings.acrylic}
                label="Translucent window"
                onchange={(on) => settings.setAcrylic(on)}
              />
            </SettingRow>
          </section>

          <section class="well flex flex-col divide-y divide-line/70 px-1 py-1">
            <h3 class="px-2.5 pb-1 pt-2 text-sm font-semibold">Titlebar</h3>
            <SettingRow
              label="Show what cortisoff costs"
              description="CPU share and memory of the daemon and this window, every two seconds."
            >
              <Switch
                checked={settings.usage}
                label="Show what cortisoff costs"
                onchange={(on) => settings.setUsage(on)}
              />
            </SettingRow>
          </section>

          <section class="well flex flex-col divide-y divide-line/70 px-1 py-1">
            <h3 class="px-2.5 pb-1 pt-2 text-sm font-semibold">Startup</h3>
            <SettingRow
              label="Start when you log on"
              description={autostart === null
                ? "Task Scheduler did not answer."
                : "A logon task, no service and no admin."}
            >
              <Switch
                checked={autostart ?? false}
                disabled={autostart === null}
                label="Start cortisoff when you log on"
                onchange={setAutostart}
              />
            </SettingRow>
          </section>
        </div>
      {:else if current === "machine"}
        <div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
          {#if dirty.length}
            <p class="rounded-card border border-line px-3 py-2 text-[13px]">
              <span class="text-danger">A previous run did not finish restoring.</span>
              <span class="mono text-foreground-muted">{dirty.join(", ")}</span>
              <span class="text-foreground-muted">
                goes back the next time the daemon starts, before any plugin fires.
              </span>
            </p>
          {/if}

          {#each warnings as warning (warning)}
            <p
              class="rounded-card border border-line px-3 py-2 text-[13px] leading-relaxed
                text-foreground-muted"
            >
              {warning}
            </p>
          {/each}

          <section class="well px-1 py-1">
            <h3 class="flex items-baseline gap-2 px-2.5 pb-1 pt-2">
              <span class="text-sm font-semibold">Capabilities</span>
              <span class="text-xs text-foreground-muted">
                {present.length} of {capabilities.length} on this machine
              </span>
            </h3>
            <ul class="flex flex-col">
              {#each capabilities as cap (cap.id)}
                <li
                  class="flex items-baseline gap-2 border-t border-line/70 px-2.5 py-2 text-[13px]"
                  title={cap.missing_because ?? cap.id}
                >
                  <span
                    class="min-w-0 flex-1 truncate {cap.missing_because
                      ? 'text-foreground-subtle'
                      : ''}"
                  >
                    {cap.label}
                  </span>
                  {#if cap.missing_because}
                    <span class="shrink-0 text-xs text-foreground-subtle">unavailable</span>
                  {:else}
                    {#if !cap.settable}
                      <span class="shrink-0 text-xs text-foreground-subtle">read only</span>
                    {/if}
                    <span class="mono shrink-0 text-xs text-foreground-subtle">
                      {cap.backend ?? ""}
                    </span>
                  {/if}
                </li>
              {:else}
                <li class="py-4 text-center text-[13px] text-foreground-muted">
                  start cortisoffd to see what this machine can do
                </li>
              {/each}
            </ul>
          </section>
        </div>
        <footer class="flex shrink-0 items-center gap-2 border-t border-line px-4 py-2.5">
          <Button
            variant="quiet"
            onclick={() => daemon.reload()}
            disabled={!daemon.connected || daemon.busy}
            title="Re-read the plugins file, in case you edited it by hand"
          >
            Re-read the file
          </Button>
          <div class="ml-auto">
            <Button
              variant="danger"
              onclick={() => daemon.stopDaemon()}
              disabled={!daemon.connected || daemon.busy}
              title="Stops the daemon and puts every screen back. This window stays open."
            >
              Stop the daemon
            </Button>
          </div>
        </footer>
      {:else}
        <div
          bind:this={logBox}
          onscroll={onLogScroll}
          class="mono min-h-0 flex-1 overflow-y-auto px-4 py-3 text-xs leading-relaxed"
        >
          {#if daemon.trace.length === 0}
            <p class="py-6 text-center text-foreground-subtle">
              nothing yet. this fills as soon as something happens
            </p>
          {:else}
            {#each daemon.trace as line (line.id)}
              <div class="flex gap-2">
                <span class="shrink-0 text-foreground-subtle">{line.at}</span>
                <span
                  class="w-14 shrink-0 {line.kind === 'hold' || line.kind === 'closing'
                    ? 'text-foreground'
                    : 'text-foreground-subtle'}"
                >
                  {line.kind}
                </span>
                <span
                  class="min-w-0 flex-1 break-words {line.level === 'warn' ||
                  line.level === 'error'
                    ? 'text-danger'
                    : 'text-foreground-muted'}"
                >
                  {line.text}
                </span>
              </div>
            {/each}
          {/if}
        </div>
        <footer class="flex shrink-0 items-center gap-2 border-t border-line px-4 py-2.5">
          <span class="text-xs text-foreground-muted">{daemon.trace.length} lines</span>
          <div class="ml-auto">
            <Button
              variant="quiet"
              onclick={() => daemon.clearTrace()}
              disabled={!daemon.trace.length}
            >
              Clear
            </Button>
          </div>
        </footer>
      {/if}
    </section>
  </div>
</div>
