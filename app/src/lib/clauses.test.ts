import { describe, expect, it } from "vitest";

import { fromClauses, toClauses } from "./clauses";
import type { Condition } from "./types";

/**
 * The dialog turns a condition into rows and back. A bug here silently
 * rewrites a rule someone wrote, which is the one failure this application
 * must not have: the rule file is the thing a user cannot rebuild from memory.
 */
function roundTrip(condition: Condition) {
  const clauses = toClauses(condition);
  expect(clauses, "should be editable as rows").not.toBeNull();
  expect(fromClauses(clauses!)).toEqual(condition);
}

describe("a condition that fits the rows", () => {
  it("survives the trip through the dialog", () => {
    roundTrip({ when: "always" });
    roundTrip({ when: "idle", value: true });
    roundTrip({ when: "fullscreen", value: false });
    roundTrip({ when: "not", value: { when: "maximized", value: true } });
    roundTrip({ when: "app", value: { exe: "cs2.exe", title_contains: null } });
    roundTrip({ when: "between", value: { start: "20:00", end: "07:00" } });
    roundTrip({
      when: "any",
      value: [
        { when: "app", value: { exe: "vlc.exe", title_contains: null } },
        { when: "app", value: { exe: "mpv.exe", title_contains: null } },
      ],
    });
    roundTrip({
      when: "not",
      value: { when: "app", value: { exe: "vlc.exe", title_contains: null } },
    });
  });

  /// The rule the daemon seeds on a first run, which is the one every new user
  /// opens first.
  it("survives the shipped starter rule", () => {
    roundTrip({
      when: "all",
      value: [
        { when: "between", value: { start: "20:00", end: "07:00" } },
        {
          when: "not",
          value: {
            when: "any",
            value: [
              { when: "app", value: { exe: "vlc.exe", title_contains: null } },
              { when: "app", value: { exe: "mpc-hc64.exe", title_contains: null } },
            ],
          },
        },
      ],
    });
  });

  /// An app condition naming both an executable and a title is two rows, and
  /// comes back as the `all` of two conditions that means the same thing.
  it("splits an application and a title into two rows", () => {
    const clauses = toClauses({
      when: "app",
      value: { exe: "chrome.exe", title_contains: "youtube" },
    });
    expect(clauses).toHaveLength(2);
    expect(fromClauses(clauses!)).toEqual({
      when: "all",
      value: [
        { when: "app", value: { exe: "chrome.exe", title_contains: null } },
        { when: "app", value: { exe: null, title_contains: "youtube" } },
      ],
    });
  });
});

describe("a condition that does not fit the rows", () => {
  /// Answering null is what makes the dialog show it read-only instead of
  /// flattening it into something the person never wrote.
  it("is refused rather than mangled", () => {
    const nested: Condition = {
      when: "any",
      value: [
        { when: "idle", value: true },
        { when: "between", value: { start: "20:00", end: "07:00" } },
      ],
    };
    expect(toClauses(nested)).toBeNull();

    const doubleNegative: Condition = {
      when: "not",
      value: { when: "not", value: { when: "idle", value: true } },
    };
    expect(toClauses(doubleNegative)).toBeNull();

    const nestedAll: Condition = {
      when: "all",
      value: [
        { when: "idle", value: true },
        { when: "all", value: [{ when: "locked", value: false }] },
      ],
    };
    expect(toClauses(nestedAll)).toBeNull();
  });
});
