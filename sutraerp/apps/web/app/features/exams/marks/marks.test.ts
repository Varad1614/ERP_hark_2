/** Slice 6B: decimal helpers and the mock marks / malpractice flows (no floats on display). */
import { describe, expect, it } from "vitest";
import { PATHS } from "./api";
import { createMockMarksApi, fromHundredths, toHundredths } from "./mock";

describe("6B decimals", () => {
  it("round-trips marks as hundredths", () => {
    expect(toHundredths("61.5")).toBe(6150);
    expect(toHundredths("34.50")).toBe(3450);
    expect(fromHundredths(7160)).toBe("71.60");
    expect(fromHundredths(5)).toBe("0.05");
  });

  it("keeps every path under the exams API", () => {
    for (const p of Object.values(PATHS)) expect(p.startsWith("/api/v1/exams/")).toBe(true);
  });
});

describe("6B mock flows", () => {
  it("walks a mark sheet from entry to final approved marks", async () => {
    const api = createMockMarksApi();
    const [paper] = await api.myAssessments();
    expect(paper?.acceptsMarks).toBe(true);
    const sheet = await api.openSheet(paper!.paperId);
    expect(sheet.status).toBe("planned");
    await api.importInternal(sheet.id);
    let list = await api.marks(paper!.paperId);
    list = await api.enter(
      paper!.paperId,
      list.items.map((r) => ({ id: r.id, version: r.version, externalMarks: "61" })),
    );
    expect(list.items[0]?.preview?.percent).toBe("71.10");
    expect(list.items[0]?.preview?.grade).toBe("A");
    let s = await api.sheetCommand(sheet.id, list.sheet!.version, "submit");
    s = await api.sheetCommand(sheet.id, s.version, "verify");
    s = await api.sheetCommand(sheet.id, s.version, "request-approval");
    s = await api.sheetCommand(sheet.id, s.version, "finalise");
    expect(s.status).toBe("finalised");
    expect((await api.marks(paper!.paperId)).paperStatus).toBe("marks_finalised");
  });

  it("refuses a duplicate open malpractice case and walks decision → appeal → overturn", async () => {
    const api = createMockMarksApi();
    const c = await api.reportCase("s", { registrationId: "r", paperId: "p", allegation: "Notes" });
    await expect(api.reportCase("s", { registrationId: "r", paperId: "p", allegation: "Again" })).rejects.toBeTruthy();
    let x = await api.caseCommand(c.id, c.version, "investigate");
    x = await api.decideCase(x.id, x.version, "debarred", "Found notes");
    expect(x.penalty).toBe("debarred");
    x = await api.caseCommand(x.id, x.version, "appeal", "Unrelated notes");
    x = await api.reviewCase(x.id, x.version, "overturn", "Not relevant");
    expect([x.status, x.penalty]).toEqual(["overturned", "none"]);
  });
});
