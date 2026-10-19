import { describe, expect, it } from "vitest";
import { ACADEMICS_TABS, activeTab, CAMPUS_SETUP_TABS } from "./index";

describe("Batch 2 admin tabs", () => {
  it("defaults to the first tab and resolves known keys", () => {
    expect(activeTab(ACADEMICS_TABS, undefined).key).toBe("structure");
    expect(activeTab(ACADEMICS_TABS, "calendar").key).toBe("calendar");
    expect(activeTab(CAMPUS_SETUP_TABS, "bogus").key).toBe("institution");
    expect(new Set(CAMPUS_SETUP_TABS.map((t) => t.key)).size).toBe(CAMPUS_SETUP_TABS.length);
  });
});
