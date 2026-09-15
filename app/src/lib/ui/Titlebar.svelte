<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  import { daemon } from "$lib/daemon.svelte";
  import activeLogo from "$lib/assets/logo-active.svg";
  import inactiveLogo from "$lib/assets/logo-inactive.svg";
  import { settings } from "$lib/settings.svelte";
  import { updater } from "$lib/updater.svelte";

  /**
   * The window is undecorated, so this strip is the titlebar.
   *
   * `data-tauri-drag-region` has to sit on the element itself: a child that
   * swallows the press stops the drag, which is why the label and the status
   * carry it too. The caption buttons run the full height and sit flush
   * against the right edge, the way the OS draws its own.
   *
   * The usage readout is off by default and polled only while it is on: a
   * window that wakes twice a second to measure itself is the thing it would
   * be measuring.
   */
  let { onsettings }: { onsettings: () => void } = $props();

  type Usage = { pid: number; cpu: number | null; rss_bytes: number };

  const win = getCurrentWindow();
  const active = $derived(daemon.connected && (daemon.state?.active ?? false));
  let usage = $state<Usage[]>([]);

  $effect(() => {
    if (!settings.usage) {
      usage = [];
      return;
    }
    let alive = true;
    const sample = async () => {
      const pids = daemon.doctor?.pid ? [0, daemon.doctor.pid] : [0];
      try {
        const next = await invoke<Usage[]>("usage", { pids });
        if (alive) usage = next;
      } catch {
        if (alive) usage = [];
      }
    };
    void sample();
    const timer = setInterval(sample, 2000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  });

  const window_ = $derived(usage.find((u) => u.pid === 0) ?? null);
  const daemon_ = $derived(usage.find((u) => u.pid !== 0) ?? null);

  function megabytes(bytes: number): string {
    return `${Math.round(bytes / 1_048_576)} MB`;
  }

  /** Null on the first sample, when there is no interval to divide by yet. */
  function cost(u: Usage): string {
    const memory = megabytes(u.rss_bytes);
    if (u.cpu === null) return memory;
    const cpu = u.cpu < 10 ? u.cpu.toFixed(1) : String(Math.round(u.cpu));
    return `${cpu}% · ${memory}`;
  }
</script>

<header
  data-tauri-drag-region
  class="flex h-(--titlebar-height) shrink-0 select-none items-center gap-3 border-b border-line
    pl-3"
>
  <img data-tauri-drag-region src={active ? activeLogo : inactiveLogo}
    alt={active ? "Cortisoff active" : "Cortisoff inactive"} class="size-6" />
  <span data-tauri-drag-region class="text-[13px] font-semibold tracking-tight">cortisoff</span>

  <span
    data-tauri-drag-region
    class="flex items-center gap-1.5 text-xs {daemon.connected
      ? 'text-foreground-muted'
      : 'text-danger'}"
  >
    <span
      class="size-1.5 rounded-full {daemon.connected ? 'bg-success' : 'bg-danger'}"
    ></span>
    {daemon.connected ? "running" : (daemon.reason ?? "not running")}
  </span>

  {#if settings.usage}
    <!-- What the two processes cost, right where the eye already is. -->
    <span
      data-tauri-drag-region
      class="num ml-1 flex items-center gap-3 text-xs text-foreground-subtle"
      title="CPU is a share of the whole machine, memory is the working set"
    >
      {#if daemon_}
        <span data-tauri-drag-region>
          <span class="text-foreground-muted">daemon</span>
          {cost(daemon_)}
        </span>
      {/if}
      {#if window_}
        <span data-tauri-drag-region>
          <span class="text-foreground-muted">window</span>
          {cost(window_)}
        </span>
      {/if}
    </span>
  {/if}

  <div class="ml-auto flex h-full items-stretch">
    <button
      type="button"
      aria-label="Usage"
      aria-pressed={settings.usage}
      title={settings.usage ? "Hide what cortisoff costs" : "Show what cortisoff costs"}
      class="grid w-9 place-items-center transition-colors hover:bg-muted hover:text-foreground
        {settings.usage ? 'text-foreground' : 'text-foreground-subtle'}"
      onclick={() => settings.setUsage(!settings.usage)}
    >
      <svg
        width="13"
        height="13"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
      </svg>
    </button>

    <button
      type="button"
      aria-label="Settings"
      title="Settings, this machine and the logs"
      class="relative grid w-9 place-items-center text-foreground-muted transition-colors
        hover:bg-muted hover:text-foreground"
      onclick={onsettings}
    >
      <svg
        width="13"
        height="13"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
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
      <!-- The only badge in the window. An update waiting is the one thing
           worth pulling someone into a dialog they did not ask for. -->
      {#if updater.stage === "ready"}
        <span class="absolute right-1.5 top-2 size-1.5 rounded-full bg-success"></span>
      {/if}
    </button>

    <button
      type="button"
      aria-label="Minimise"
      class="grid w-11 place-items-center text-foreground-muted transition-colors hover:bg-muted
        hover:text-foreground"
      onclick={() => win.minimize()}
    >
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
        <path d="M0 5h10" stroke="currentColor" stroke-width="1" />
      </svg>
    </button>
    <button
      type="button"
      aria-label="Close"
      class="grid w-11 place-items-center text-foreground-muted transition-colors hover:bg-danger
        hover:text-white"
      onclick={() => win.close()}
    >
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
        <path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1" />
      </svg>
    </button>
  </div>
</header>
