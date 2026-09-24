import { describe, expect, it } from "vitest";

import { isSwitch, kelvinRgb, numberOf, rangeFor, startingText, switchedOn, withUnit } from "./ranges";
import type { Control } from "./types";

const kelvin: Control = { kind: "range", min: 1500, max: 6600, step: 50, default: 3400, unit: "K" };
const percent: Control = { kind: "range", min: 0, max: 100, step: 1, default: 50, unit: "%" };

describe("a colour temperature track", () => {
  /// The same curve the daemon tints with: warm is orange, daylight is white,
  /// and blue only ever rises with the temperature.
  it("goes from orange to white, blue rising the whole way", () => {
    const [r, g, b] = kelvinRgb(1500);
    expect(r).toBe(255);
    expect(g).toBeLessThan(140);
    expect(b).toBe(0);
    expect(kelvinRgb(6600)).toEqual([255, 255, 255]);

    let last = -1;
    for (let k = 1500; k <= 6600; k += 100) {
      const blue = kelvinRgb(k)[2];
      expect(blue).toBeGreaterThanOrEqual(last);
      last = blue;
    }
  });

  it("is drawn only for a value in kelvin", () => {
    expect(rangeFor(kelvin)?.track).toContain("linear-gradient");
    expect(rangeFor(percent)?.track).toBeUndefined();
  });
});

describe("a slider's value", () => {
  it("reads the rule's number and clamps it to the track the daemon reported", () => {
    const range = rangeFor(kelvin)!;
    expect(numberOf("3200", range)).toBe(3200);
    expect(numberOf(" 9000 ", range)).toBe(6600);
    expect(numberOf("", range)).toBe(3400);
    expect(numberOf("warm", range)).toBe(3400);
    expect(range.text(3200)).toBe("3200 K");
  });

  it("offers no track for a control that is not a number", () => {
    expect(rangeFor({ kind: "text", hint: "@240" })).toBeNull();
    expect(rangeFor({ kind: "switch" })).toBeNull();
    expect(rangeFor(null)).toBeNull();
    expect(isSwitch({ kind: "switch" })).toBe(true);
    expect(isSwitch(kelvin)).toBe(false);
  });

  it("puts a unit where a person expects it", () => {
    expect(withUnit(50, "%")).toBe("50%");
    expect(withUnit(3400, "K")).toBe("3400 K");
    expect(withUnit(7, null)).toBe("7");
  });
});

describe("a capability picked on a row", () => {
  it("starts from its own default, never the previous capability's value", () => {
    expect(startingText(kelvin)).toBe("3400");
    expect(startingText({ kind: "switch" })).toBe("true");
    expect(startingText({ kind: "text", hint: "@240" })).toBe("");
    expect(startingText(null)).toBe("");
  });

  it("reads a switch the way the rule file spells it", () => {
    expect(switchedOn("true")).toBe(true);
    expect(switchedOn("false")).toBe(false);
    expect(switchedOn(" off ")).toBe(false);
    expect(switchedOn("on")).toBe(true);
  });
});
