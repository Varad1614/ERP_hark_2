/**
 * Batch 1D self-service screens in mock mode: notifications, to-dos, notices, profile, security,
 * and the app-shell bell / account menu. Each test re-imports the modules so the per-role mock
 * inbox starts fresh.
 */
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ROLE_CONFIG } from "../lib/roles";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});
afterEach(() => {
  vi.unstubAllGlobals();
});

async function harness() {
  return (await import("../test/harness")).renderScreen;
}

describe("notifications screen", () => {
  it("lists notifications, filters unread and marks all read", async () => {
    const renderScreen = await harness();
    const { default: Notifications } = await import("./notifications");
    await renderScreen(<Notifications />, { path: "/notifications" });
    const titles = ROLE_CONFIG.administrator.notifications.map((n) => n.title);
    for (const t of titles) expect(await screen.findByText(t)).toBeTruthy();
    const first = document.querySelector("[data-notification]");
    if (!first) throw new Error("no rows");
    fireEvent.click(first);
    await waitFor(() => expect(document.querySelectorAll("[data-notification].is-read")).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: /Unread/ }));
    await waitFor(() => expect(document.querySelectorAll("[data-notification]")).toHaveLength(titles.length - 1));
    fireEvent.click(screen.getByRole("button", { name: /Mark all read/ }));
    expect(await screen.findByText("You’re all caught up")).toBeTruthy();
  });
});

describe("to-dos screen", () => {
  it("completes a to-do and shows it under Done", async () => {
    const renderScreen = await harness();
    const { default: Todos } = await import("./todos");
    await renderScreen(<Todos />, { path: "/todos", role: "faculty" });
    const title = ROLE_CONFIG.faculty.todos[0]?.title ?? "";
    const row = (await screen.findByText(title)).closest("[data-todo]") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByText(title)).toBeNull());
    const doneFilter = [...document.querySelectorAll<HTMLButtonElement>("button.sb-filter")].find((b) => b.textContent?.startsWith("Done"));
    if (!doneFilter) throw new Error("no Done filter");
    fireEvent.click(doneFilter);
    expect(await screen.findByText(title)).toBeTruthy();
  });

  it("validates and adds a personal to-do", async () => {
    const renderScreen = await harness();
    const { default: Todos } = await import("./todos");
    await renderScreen(<Todos />, { path: "/todos", role: "student" });
    fireEvent.click(await screen.findByRole("button", { name: "New to-do" }));
    const form = document.querySelector("[data-todo-form]") as HTMLFormElement;
    fireEvent.submit(form);
    expect(await screen.findByText("What needs doing?")).toBeTruthy();
    fireEvent.change(form.querySelector('input[name="title"]') as HTMLInputElement, { target: { value: "Return library books" } });
    fireEvent.submit(form);
    expect(await screen.findByText("Return library books")).toBeTruthy();
  });
});

describe("notices", () => {
  it("validates the form like the API", async () => {
    const { validateNotice } = await import("./notices");
    expect(validateNotice({ title: "", body: "", audienceRoles: [] })).toEqual({
      title: "Give the notice a title",
      body: "Write the announcement",
      audienceRoles: "Choose at least one audience",
    });
    expect(validateNotice({ title: "t", body: "b", audienceRoles: ["student"], publishAt: "2030-01-02", expiresAt: "2030-01-01" }).expiresAt).toBeTruthy();
    expect(validateNotice({ title: "t", body: "b", audienceRoles: ["student"] })).toEqual({});
  });

  it("shows the current notice to its audience", async () => {
    const renderScreen = await harness();
    const { default: Notices } = await import("./notices");
    await renderScreen(<Notices />, { path: "/notices", role: "student" });
    expect(await screen.findByText("Innovation Week begins 23 September")).toBeTruthy();
  });
});

describe("profile", () => {
  it("validates contact details and sends only changed members", async () => {
    const { validateProfile, profilePatch } = await import("./profile");
    const draft = { displayName: " ", locale: "en-IN", timeZone: "Asia/Kolkata", contactEmail: "nope", contactPhone: "98765" };
    expect(Object.keys(validateProfile(draft)).sort()).toEqual(["contactEmail", "contactPhone", "displayName"]);
    const { mockInbox } = await import("../lib/inbox");
    const p = await mockInbox("student").profile();
    const prefs = p.notificationPreferences.map((x) => (x.category === "notice" ? { ...x, email: true } : x));
    expect(
      profilePatch(p, { displayName: p.displayName, locale: p.locale, timeZone: "UTC", contactEmail: "", contactPhone: "+919876543210" }, prefs),
    ).toEqual({ timeZone: "UTC", contactPhone: "+919876543210", notificationPreferences: [{ category: "notice", inApp: true, email: true }] });
  });

  it("saves an edited display name", async () => {
    const renderScreen = await harness();
    const { default: ProfilePage } = await import("./profile");
    await renderScreen(<ProfilePage />, { path: "/profile", role: "student" });
    const name = (await screen.findByLabelText("Display name")) as HTMLInputElement;
    expect(name.value).toBe(ROLE_CONFIG.student.name);
    fireEvent.change(name, { target: { value: "Aarav Kulkarni" } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect((screen.getByLabelText("Display name") as HTMLInputElement).value).toBe("Aarav Kulkarni"));
  });
});

describe("security", () => {
  it("describes how a session signed in", async () => {
    const { describeSession } = await import("./security");
    expect(describeSession({ amr: ["pwd"], federated: false, mfaLevel: "none" })).toBe("Password");
    expect(describeSession({ amr: ["pwd", "hwk", "mfa"], federated: false, mfaLevel: "mfa" })).toBe("Password + passkey");
    expect(describeSession({ amr: ["pwd", "otp"], federated: false, mfaLevel: "mfa" })).toBe("Password + authenticator");
    expect(describeSession({ amr: [], federated: true, mfaLevel: "mfa" })).toBe("Single sign-on + second factor");
  });

  it("signs out the other sessions", async () => {
    const renderScreen = await harness();
    const { default: Security } = await import("./security");
    await renderScreen(<Security />, { path: "/account/security" });
    await waitFor(() => expect(document.querySelectorAll("[data-session]")).toHaveLength(2));
    expect(screen.getByText("This device")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Sign out others" }));
    await waitFor(() => expect(document.querySelectorAll("[data-session]")).toHaveLength(1));
    expect(await screen.findByText(/Authenticator app/)).toBeTruthy();
  });
});

describe("app shell", () => {
  it("bell shows the unread count and real notifications; menu links Security and To-dos", async () => {
    const renderScreen = await harness();
    const { default: AppShell } = await import("./app-shell");
    const { router } = await renderScreen(<AppShell />, { path: "/" });
    const total = ROLE_CONFIG.administrator.notifications.length;
    const bell = await screen.findByRole("button", { name: `Notifications, ${total} unread` });
    fireEvent.click(bell);
    const first = ROLE_CONFIG.administrator.notifications[0]?.title ?? "";
    expect(await screen.findByText(first)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Mark all read" }));
    expect(await screen.findByRole("button", { name: "Notifications, 0 unread" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Account menu" }));
    expect(document.querySelector("[data-todos-menu]")).toBeTruthy();
    fireEvent.click(document.querySelector("[data-security-menu]") as HTMLElement);
    await waitFor(() => expect(router.state.location.pathname).toBe("/account/security"));
  });
});
