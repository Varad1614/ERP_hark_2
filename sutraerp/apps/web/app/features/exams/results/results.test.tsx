/** Slice 6C web: mock API behaviour, HTTP request shape and the student Results panel. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createHttpResultsApi, PATHS } from "./api";
import { createMockResultsApi } from "./mock";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("6C results API", () => {
  it("refuses a second live review of the same paper (mock)", async () => {
    const api = createMockResultsApi();
    const [result] = await api.myResults();
    const paperId = result?.lines[0]?.paperId ?? "";
    const first = await api.requestReview({ paperId, kind: "rechecking" });
    expect(first.status).toBe("fee_pending");
    expect(first.feeAmount).toBe("200.00");
    await expect(api.requestReview({ paperId, kind: "revaluation" })).rejects.toThrow();
    const withdrawn = await api.withdrawMyReview(first.id);
    expect(withdrawn.status).toBe("withdrawn");
  });

  it("sends If-Match and Idempotency-Key on commands", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "b1", status: "computed", version: 3 }), { status: 200 });
    };
    const api = createHttpResultsApi(fetchImpl, "");
    await api.batchCommand("b1", 2, "compute");
    const headers = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.compute.replace("{id}", "b1"));
    expect(headers["If-Match"]).toBe('"2"');
    expect(headers["Idempotency-Key"]).toBeTruthy();
  });
});

describe("Student Results panel", () => {
  it("shows the published result with SGPA / CGPA as strings", async () => {
    const { renderScreen } = await import("../../../test/harness");
    const { StudentResultsPanel } = await import("./index");
    await renderScreen(<StudentResultsPanel />, { role: "student", path: "/results", entry: "/results" });
    expect(await screen.findByText(/SGPA 7\.86/)).toBeTruthy();
    expect(document.querySelector("[data-slice='6C']")).toBeTruthy();
  });
});
