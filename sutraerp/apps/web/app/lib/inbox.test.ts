import { describe, expect, it } from "vitest";
import { mockInbox, relativeTime } from "./inbox";
import { ROLE_CONFIG } from "./roles";

describe("mock inbox", () => {
  it("counts unread notifications and marks them read", async () => {
    const inbox = mockInbox("administrator");
    const total = ROLE_CONFIG.administrator.notifications.length;
    expect(await inbox.unreadCount()).toBe(total);
    const page = await inbox.notifications();
    expect(page.items).toHaveLength(total);
    const first = page.items[0];
    if (!first) throw new Error("no notification");
    await inbox.markRead(first.id);
    expect(await inbox.unreadCount()).toBe(total - 1);
    expect((await inbox.notifications({ unreadOnly: true })).items.map((n) => n.id)).not.toContain(first.id);
    expect(await inbox.markAllRead()).toBe(total - 1);
    expect(await inbox.unreadCount()).toBe(0);
  });

  it("moves to-dos between open, completed and dismissed", async () => {
    const inbox = mockInbox("faculty");
    const open = await inbox.todos("open");
    const t = open.items[0];
    if (!t) throw new Error("no to-do");
    expect((await inbox.setTodo(t.id, "complete")).status).toBe("completed");
    expect((await inbox.todos("open")).openCount).toBe(open.openCount - 1);
    expect((await inbox.todos("completed")).items.map((x) => x.id)).toContain(t.id);
    expect((await inbox.setTodo(t.id, "dismiss")).dismissedAt).toBeTruthy();
    expect((await inbox.setTodo(t.id, "reopen")).status).toBe("open");
    const created = await inbox.createTodo({ title: "Grade lab 4" });
    expect((await inbox.todos("open")).items[0]?.id).toBe(created.id);
  });

  it("shows notices to their audience only and schedules future ones", async () => {
    const admin = mockInbox("administrator");
    const future = new Date(Date.now() + 3_600_000).toISOString();
    const scheduled = await admin.createNotice({ title: "Later", body: "b", audienceRoles: ["student"], publishAt: future });
    expect(scheduled.state).toBe("scheduled");
    const live = await admin.createNotice({ title: "Staff meeting", body: "b", audienceRoles: ["faculty"] });
    expect(live.state).toBe("live");
    expect((await admin.notices("current")).items.map((n) => n.title)).not.toContain("Staff meeting");
    expect((await admin.notices("manage")).items.map((n) => n.title)).toContain("Staff meeting");
    expect((await admin.archiveNotice(live.id)).state).toBe("archived");
  });

  it("rejects a stale profile version and applies a patch", async () => {
    const inbox = mockInbox("student");
    const p = await inbox.profile();
    const next = await inbox.updateProfile(p.version, { displayName: "Aarav K.", contactEmail: "aarav@example.edu" });
    expect(next.displayName).toBe("Aarav K.");
    expect(next.version).toBe(p.version + 1);
    await expect(inbox.updateProfile(p.version, { displayName: "x" })).rejects.toThrow(/reload/);
  });

  it("revokes other sessions but never the current one", async () => {
    const inbox = mockInbox("administrator");
    const all = await inbox.sessions();
    const current = all.find((s) => s.current);
    if (!current) throw new Error("no current session");
    await inbox.revokeSession(current.id);
    expect((await inbox.sessions()).some((s) => s.current)).toBe(true);
    expect(await inbox.revokeOthers()).toBe(all.length - 1);
    expect(await inbox.sessions()).toHaveLength(1);
  });
});

describe("relativeTime", () => {
  const now = Date.parse("2030-01-01T12:00:00Z");
  it("formats past and future", () => {
    expect(relativeTime("2030-01-01T12:00:20Z", now)).toBe("just now");
    expect(relativeTime("2030-01-01T11:55:00Z", now)).toBe("5 min ago");
    expect(relativeTime("2030-01-01T14:00:00Z", now)).toBe("in 2 h");
    expect(relativeTime("2029-12-29T12:00:00Z", now)).toBe("3 d ago");
  });
});
