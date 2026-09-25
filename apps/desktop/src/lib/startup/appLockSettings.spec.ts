import { describe, expect, it } from "vitest";
import { nextLockEnabled } from "./appLockSettings";

describe("app lock settings", () => {
  it("enables only after a verified prompt", () => {
    expect(nextLockEnabled({ available: true, outcome: "verified" })).toBe("enable");
    expect(nextLockEnabled({ available: true, outcome: "canceled" })).toBe("keep");
    expect(nextLockEnabled({ available: false, outcome: "verified" })).toBe("unavailable");
  });
});
