<script lang="ts">
  import { allToasts, dismissToast } from "$lib/toasts.svelte";
  import Toast from "./Toast.svelte";

  const toasts = $derived(allToasts());
</script>

<!-- Bottom right, above everything. A toast covering a card would hide the
     thing it is reporting on, so the stack grows upwards from the corner. -->
<div class="pointer-events-none absolute bottom-3 right-3 z-30 flex flex-col items-end gap-2">
  {#each toasts as t (t.id)}
    <Toast
      message={t.message}
      kind={t.kind}
      resetKey={t.resetKey}
      ondone={() => dismissToast(t.id)}
    />
  {/each}
</div>
