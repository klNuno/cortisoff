<script lang="ts">
  /**
   * A number you drag.
   *
   * A native range input, restyled: the track is a thin line filled up to the
   * thumb, the value reads at the right in the same face as everything else.
   * When the number is itself a colour, the track shows the colours instead
   * of a fill and the thumb alone says where you are.
   */
  let {
    value,
    min,
    max,
    step = 1,
    label,
    text,
    track,
    disabled = false,
    onchange,
    onrelease,
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    label: string;
    /** The value, with its unit, as it reads to a person. */
    text: string;
    /** A CSS background for the track, replacing the fill. */
    track?: string;
    disabled?: boolean;
    onchange: (next: number) => void;
    /**
     * The drag ended, by pointer or keyboard. Fires after the last
     * `onchange`, so what was previewed can be let go of.
     */
    onrelease?: () => void;
  } = $props();

  const percent = $derived(
    max > min ? ((Math.min(max, Math.max(min, value)) - min) / (max - min)) * 100 : 0,
  );
</script>

<label class="slider" class:disabled>
  <span class="head">
    <span class="name">{label}</span>
    <span class="value">{text}</span>
  </span>
  <input
    type="range"
    {min}
    {max}
    {step}
    {value}
    {disabled}
    aria-label={label}
    aria-valuetext={text}
    style:--fill="{percent}%"
    style:--track={track}
    oninput={(e) => onchange(Number(e.currentTarget.value))}
    onpointerup={() => onrelease?.()}
    onpointercancel={() => onrelease?.()}
    onkeyup={() => onrelease?.()}
  />
</label>

<style>
  .slider {
    display: flex;
    min-width: 0;
    flex-direction: column;
    gap: 4px;
  }

  .head {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
    line-height: 18px;
  }

  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-muted);
  }

  .value {
    flex-shrink: 0;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  input {
    width: 100%;
    height: 20px;
    margin: 0;
    appearance: none;
    background: transparent;
    cursor: pointer;
  }

  input:focus-visible {
    outline: none;
  }

  input::-webkit-slider-runnable-track {
    height: 4px;
    border-radius: 999px;
    background: var(
      --track,
      linear-gradient(to right, var(--fg) var(--fill), var(--bg-elevated) var(--fill))
    );
  }

  input::-webkit-slider-thumb {
    width: 14px;
    height: 14px;
    margin-top: -5px;
    appearance: none;
    border: 2px solid var(--bg-card);
    border-radius: 50%;
    background: var(--fg);
    box-shadow:
      0 0 0 1px rgb(0 0 0 / 0.45),
      0 1px 3px rgb(0 0 0 / 0.4);
    transition: transform 90ms ease-out;
  }

  input:hover::-webkit-slider-thumb,
  input:active::-webkit-slider-thumb {
    transform: scale(1.15);
  }

  input:focus-visible::-webkit-slider-thumb {
    box-shadow:
      0 0 0 1px rgb(0 0 0 / 0.45),
      0 0 0 4px color-mix(in srgb, var(--fg) 22%, transparent);
  }

  .disabled {
    opacity: 0.45;
    pointer-events: none;
  }
</style>
