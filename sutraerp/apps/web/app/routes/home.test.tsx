/**
 * Home "Your to-dos" card: mock personas in mock mode; in API mode the live open to-dos from
 * `GET /api/v1/todos` (first four, due/overdue badges), completion through the complete endpoint
 * with the CSRF header, and the loading / empty / error states.
 */
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG } from "../lib/roles";

const principal = {
  subject: "dev|admin@sutra.dev",
  displayName: "Dhruv Naik",
  loginId: "admin@sutra.dev",
  roles: ["administrator"],
  scope: { tenantId: "0192f5a0-7c3e-7000-8000-000000000001" },
};

const HOUR = 3_600_000;
function todo(n: number, extra: Record<string, unknown> = {}) {
  return {
    id: `0192f5a0-7c3e-7000-8000-00000000000${n}`,
    title: `Task ${n}`,
    detail: "",
    status: "open",
    source: "manual",
    overdue: false,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    version: 1,
    ...extra,
  };
}

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});
afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

interface Calls {
  method: string;
  path: string;
  csrf: string | null;
}

/** API mode with a scripted `/api/v1/todos`; returns the recorded calls. */
async function mountApi(todos: () => Response | Promise<Response>) {
  vi.stubEnv("VITE_AUTH_MODE", "api");
  vi.stubEnv("VITE_API_BASE", "http://localhost");
  const calls: Calls[] = [];
  vi.stubGlobal("fetch", async (input: Request) => {
    const url = new URL(input.url);
    calls.push({ method: input.method, path: url.pathname, csrf: input.headers.get("X-CSRF-Token") });
    if (url.pathname === "/api/v1/auth/session")
      return Response.json({ principal, csrfToken: "csrf-1", expiresAt: new Date(Date.now() + HOUR).toISOString(), mfaLevel: "none", mfaEnrollmentRequired: false, amr: ["pwd"] });
    if (url.pathname === "/api/v1/todos") return todos();
    const done = url.pathname.match(/^\/api\/v1\/todos\/([^/]+)\/complete$/);
    if (done && input.method === "POST") return Response.json(todo(1, { id: done[1], status: "completed" }));
    return new Response(null, { status: 204 });
  });
  const { renderScreen } = await import("../test/harness");
  const { default: Home } = await import("./home");
  const utils = await renderScreen(<Home />, { path: "/" });
  return { calls, ...utils };
}

describe("home to-dos card", () => {
  it("shows the persona's mock to-dos in mock mode", async () => {
    const { renderScreen } = await import("../test/harness");
    const { default: Home } = await import("./home");
    await renderScreen(<Home />, { path: "/", role: "faculty" });
    for (const t of ROLE_CONFIG.faculty.todos) expect(await screen.findByText(t.title)).toBeTruthy();
    expect(document.querySelector("[data-home-todos]")).toBeNull();
  });

  it("lists the first four open to-dos with due badges and completes one with CSRF", async () => {
    let items = [
      todo(1, { title: "Approve leave", dueAt: new Date(Date.now() - HOUR).toISOString(), overdue: true }),
      todo(2, { title: "Sign the circular", dueAt: new Date(Date.now() + 26 * HOUR).toISOString() }),
      todo(3, { title: "Complete the setup", source: "event" }),
      todo(4, { title: "Review timetable", dueAt: new Date(Date.now() + 10 * 24 * HOUR).toISOString() }),
      todo(5, { title: "Fifth item" }),
      todo(6, { title: "Sixth item" }),
    ];
    const { calls, router } = await mountApi(() => Response.json({ items, openCount: items.length }));
    const card = await waitFor(() => {
      const el = document.querySelector('[data-home-todos="ready"]') as HTMLElement | null;
      if (!el) throw new Error("not ready");
      return el;
    });
    expect(card.querySelectorAll("[data-home-todo]")).toHaveLength(4);
    expect(within(card).queryByText("Fifth item")).toBeNull();
    expect(within(card).getByText("+2 more open")).toBeTruthy();
    expect(document.querySelector("[data-ring]")?.getAttribute("data-ring")).toBe("0/6");
    const row = (id: number) => card.querySelector(`[data-home-todo="${todo(id).id}"]`) as HTMLElement;
    expect(within(row(1)).getByText("Overdue")).toBeTruthy();
    expect(within(row(2)).getByText(/Due (tomorrow|\d)/)).toBeTruthy();
    expect(within(row(3)).getByText("Automatic")).toBeTruthy();
    expect(within(row(4)).getByText(/^Due \d+ \w+/)).toBeTruthy();
    expect(calls.find((c) => c.path === "/api/v1/todos")?.method).toBe("GET");

    items = items.filter((t) => t.title !== "Approve leave");
    fireEvent.click(within(row(1)).getByRole("checkbox", { name: /Approve leave/ }));
    await waitFor(() => expect(screen.queryByText("Approve leave")).toBeNull());
    const post = calls.find((c) => c.method === "POST" && c.path.endsWith("/complete"));
    expect(post?.path).toBe(`/api/v1/todos/${todo(1).id}/complete`);
    expect(post?.csrf).toBe("csrf-1");
    expect(await screen.findByText("Done: Approve leave")).toBeTruthy();
    expect(screen.getByText("Fifth item")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "See all" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/todos"));
  });

  it("shows the empty state", async () => {
    await mountApi(() => Response.json({ items: [], openCount: 0 }));
    expect(await screen.findByText("All clear for today")).toBeTruthy();
    expect(document.querySelector('[data-home-todos="empty"]')).toBeTruthy();
  });

  it("shows a loading state until the to-dos arrive", async () => {
    // Several tiles read /todos (open, completed, the week): hold every request, then answer them all.
    // A request issued after the release (a tile that mounts or refetches later) is answered at
    // once; before, it stayed pending forever and the card could stay loading (flaky on slow runners).
    const releases: ((r: Response) => void)[] = [];
    let released = false;
    const answer = () => Response.json({ items: [todo(1)], openCount: 1 });
    await mountApi(() => (released ? answer() : new Promise<Response>((r) => releases.push(r))));
    expect(document.querySelector('[data-home-todos="loading"]')).toBeTruthy();
    released = true;
    releases.forEach((r) => r(answer()));
    expect(await screen.findByText("Task 1")).toBeTruthy();
  });

  it("shows an error with a retry", async () => {
    let fail = true;
    await mountApi(() =>
      fail
        ? Response.json({ type: "t", title: "Service unavailable", status: 503, errorCode: "unavailable", detail: "the database is down" }, { status: 503 })
        : Response.json({ items: [todo(1, { title: "Back again" })], openCount: 1 }),
    );
    expect(await screen.findByText("To-dos are unavailable")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("the database is down");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Back again")).toBeTruthy();
  });
});

describe("dueBadge", () => {
  it("classifies overdue, today, tomorrow and later", async () => {
    const { dueBadge } = await import("./home");
    const now = new Date(2026, 9, 12, 9, 0);
    expect(dueBadge({ dueAt: null, overdue: false }, now)).toBeNull();
    expect(dueBadge({ dueAt: new Date(2026, 9, 12, 8, 0).toISOString(), overdue: false }, now)?.label).toBe("Overdue");
    expect(dueBadge({ dueAt: new Date(2026, 9, 13, 8, 0).toISOString(), overdue: true }, now)?.label).toBe("Overdue");
    expect(dueBadge({ dueAt: new Date(2026, 9, 12, 17, 0).toISOString(), overdue: false }, now)?.kind).toBe("attention");
    expect(dueBadge({ dueAt: new Date(2026, 9, 13, 17, 0).toISOString(), overdue: false }, now)).toEqual({ kind: "review", label: "Due tomorrow" });
    expect(dueBadge({ dueAt: new Date(2026, 9, 20, 17, 0).toISOString(), overdue: false }, now)).toEqual({ kind: "neutral", label: "Due 20 Oct" });
  });
});
