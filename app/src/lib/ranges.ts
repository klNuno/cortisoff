/**
 * What a slider knows about a capability.
 *
 * Nothing, until the daemon says. Every actuator reports a `Control`, and a
 * numeric one carries its ends, its step, its default and its unit, so the
 * dashboard builds the track from that and never keeps a table of its own.
 * The one thing decided here is that a value in kelvin is a colour, and a
 * colour is shown rather than filled.
 */
import type { Control } from "./types";

export type Range = {
  min: number;
  max: number;
  step: number;
  /** The value the slider starts at when the rule has none yet. */
  fallback: number;
  text: (value: number) => string;
  /** A CSS background for the track, when the value itself is a colour. */
  track?: string;
};

/** The slider a control asks for, or null for anything that is not a number. */
export function rangeFor(control: Control | null | undefined): Range | null {
  if (!control || control.kind !== "range") return null;
  const { min, max, step, unit } = control;
  return {
    min,
    max,
    step: step > 0 ? step : 1,
    fallback: control.default,
    text: (v) => withUnit(v, unit),
    track: unit === "K" ? kelvinTrack(min, max) : undefined,
  };
}

/** `3400` and `K` read as `3400 K`; a percent sign sits against its number. */
export function withUnit(value: number, unit: string | null | undefined): string {
  if (!unit) return String(value);
  return unit === "%" ? `${value}%` : `${value} ${unit}`;
}

export function isSwitch(control: Control | null | undefined): boolean {
  return control?.kind === "switch";
}

/**
 * The text a freshly picked capability starts with. Picking another
 * capability on a row starts over from this: carrying the old text across is
 * how a kelvin value ended up as a wallpaper switch.
 */
export function startingText(control: Control | null | undefined): string {
  if (isSwitch(control)) return "true";
  const range = rangeFor(control);
  return range ? String(range.fallback) : "";
}

/** Whether a switch's text reads as on, spelled any way the rule file takes. */
export function switchedOn(text: string): boolean {
  return !["false", "off", "no", "0"].includes(text.trim().toLowerCase());
}

/** A value as the rule file spells it, or the range's fallback when it is not a number. */
export function numberOf(text: string, range: Range): number {
  const n = Number(text.trim());
  if (!text.trim() || !Number.isFinite(n)) return range.fallback;
  return Math.min(range.max, Math.max(range.min, n));
}

/**
 * Tanner Helland's blackbody approximation, the same one the daemon turns a
 * kelvin value into a tint with, so the track shows the colour the screen
 * will actually take.
 */
export function kelvinRgb(kelvin: number): [number, number, number] {
  const t = Math.min(40000, Math.max(1000, kelvin)) / 100;
  const r = t <= 66 ? 255 : 329.69873 * Math.pow(t - 60, -0.13320476);
  const g = t <= 66 ? 99.4708 * Math.log(t) - 161.11957 : 288.12216 * Math.pow(t - 60, -0.07551485);
  const b = t >= 66 ? 255 : t <= 19 ? 0 : 138.51773 * Math.log(t - 10) - 305.0448;
  const clamp = (v: number) => Math.round(Math.min(255, Math.max(0, v)));
  return [clamp(r), clamp(g), clamp(b)];
}

export function kelvinHex(kelvin: number): string {
  return `#${kelvinRgb(kelvin)
    .map((c) => c.toString(16).padStart(2, "0"))
    .join("")}`;
}

/** The warm-to-cool gradient a colour temperature track shows, one stop every 300 K. */
export function kelvinTrack(min: number, max: number): string {
  const stops: string[] = [];
  for (let k = min; k <= max; k += 300) {
    const at = ((k - min) / (max - min)) * 100;
    stops.push(`${kelvinHex(k)} ${at.toFixed(1)}%`);
  }
  stops.push(`${kelvinHex(max)} 100%`);
  return `linear-gradient(to right, ${stops.join(", ")})`;
}
