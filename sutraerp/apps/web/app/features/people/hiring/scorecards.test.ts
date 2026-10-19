import { describe, expect, it } from "vitest";
import { createMockHiringApi } from "./mock";
import { keyFor, leaningLabel, ratingText, tallyText, toRating } from "./scorecards";

describe("scorecard helpers", () => {
  it("words ratings, tallies and keys", () => {
    expect(ratingText(4.25)).toBe("4.3 / 5");
    expect(ratingText(null)).toBe("—");
    expect(leaningLabel("split")).toBe("Split panel");
    expect(keyFor(" Teaching demo! ")).toBe("teaching_demo");
    expect(toRating("6")).toBeNull();
    expect(toRating("3")).toBe(3);
    expect(tallyText({ count: 2, average: 4, byField: {}, recommendations: { strong_yes: 1, yes: 0, no: 1, strong_no: 0 }, leaning: "hire" })).toBe("1 strong yes · 1 no");
  });
});

describe("mock hiring api", () => {
  it("rolls scorecards into a leaning and flags missing feedback", async () => {
    const api = createMockHiringApi();
    const itv = await api.scheduleInterview("cand-1", { type: "onsite", date: "2026-01-05", start: "10:00", end: "11:00", interviewers: ["Dr. Rao"] });
    expect((await api.feedbackDue("inst")).map((r) => r.interviewId)).toContain(itv.id);
    await api.createScorecard("cand-1", { interviewId: itv.id, authorName: "Dr. Rao", recommendation: "strong_yes", ratings: { skills: 5, experience: 3 } });
    await api.createScorecard("cand-1", { authorName: "Meera", recommendation: "no", ratings: { skills: 4 } });
    const f = await api.feedback("cand-1");
    expect(f.summary.leaning).toBe("hire");
    expect(f.summary.byField.skills).toBe(4.5);
    expect(f.summary.average).toBe(4);
    expect((await api.feedbackDue("inst")).length).toBe(0);
    await expect(api.createScorecard("cand-1", { authorName: "", recommendation: "yes", ratings: {} })).rejects.toThrow(/reviewer/);
  });
});
