import { describe, expect, it } from "vitest";
import { activeTab, STUDENT_360_TABS, student360Path, student360TabsFor, STUDENTS_TABS } from "./index";

describe("Batch 3 student tabs", () => {
  it("defaults to the first tab, resolves known keys and keeps keys unique", () => {
    expect(activeTab(STUDENTS_TABS, undefined).key).toBe("directory");
    expect(activeTab(STUDENTS_TABS, "guardians").key).toBe("guardians");
    expect(activeTab(STUDENT_360_TABS, "bogus").key).toBe("overview");
    for (const tabs of [STUDENTS_TABS, STUDENT_360_TABS]) {
      expect(new Set(tabs.map((t) => t.key)).size).toBe(tabs.length);
    }
    expect(student360Path("s1")).toBe("/students/record/s1");
    expect(student360Path("s1", "documents")).toBe("/students/record/s1/documents");
  });

  it("hides the admin-only Student 360 sections from faculty (Audit C3)", () => {
    expect(student360TabsFor("administrator")).toEqual(STUDENT_360_TABS);
    const faculty = student360TabsFor("faculty").map((t) => t.key);
    expect(faculty).toEqual(["overview", "academics", "attendance", "fees"]);
    expect(activeTab(student360TabsFor("faculty"), "registration").key).toBe("overview");
    expect(activeTab(student360TabsFor("faculty"), "completion").key).toBe("overview");
  });
});
