/**
 * Data layer of the Batch 1D self-service screens: notifications, notices, to-dos, profile and
 * the security page. One interface, two implementations:
 *
 * - `apiInbox` — the typed client against sutra-api (`VITE_AUTH_MODE=api|oidc`); unsafe calls get
 *   `X-CSRF-Token` from the client middleware, creates carry an `Idempotency-Key`, updates an
 *   `If-Match` version.
 * - `mockInbox(role)` — an in-memory stand-in seeded from the role fixtures for `mock` mode, so
 *   the screens stay explorable without the API.
 */
import { newIdempotencyKey } from "@sutra/api-client";
import type {
  MfaStatus,
  NewNotice,
  NewTodo,
  Notice,
  NoticeList,
  Notification,
  NotificationPage,
  Profile,
  SessionView,
  Todo,
  TodoPage,
  UpdateProfile,
} from "@sutra/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo } from "react";
import { api, unwrap, USES_API } from "./api";
import { useSession } from "./auth";
import { ANNOUNCEMENT, ROLE_CONFIG, type Role } from "./roles";

export type TodoFilter = "open" | "completed" | "dismissed" | "all";
export type TodoAction = "complete" | "dismiss" | "reopen";

export interface InboxApi {
  notifications(q?: { unreadOnly?: boolean; cursor?: string; limit?: number }): Promise<NotificationPage>;
  unreadCount(): Promise<number>;
  markRead(id: string): Promise<void>;
  markAllRead(): Promise<number>;
  notices(view?: "current" | "manage"): Promise<NoticeList>;
  createNotice(n: NewNotice): Promise<Notice>;
  archiveNotice(id: string): Promise<Notice>;
  todos(status?: TodoFilter): Promise<TodoPage>;
  createTodo(t: NewTodo): Promise<Todo>;
  setTodo(id: string, action: TodoAction): Promise<Todo>;
  profile(): Promise<Profile>;
  updateProfile(version: number, patch: UpdateProfile): Promise<Profile>;
  sessions(): Promise<SessionView[]>;
  revokeSession(id: string): Promise<void>;
  revokeOthers(): Promise<number>;
  mfa(): Promise<MfaStatus>;
}

function noContent(r: { error?: unknown; response: Response }) {
  if (r.error !== undefined || !r.response.ok) unwrap({ error: r.error ?? { status: r.response.status } });
}

export const apiInbox: InboxApi = {
  async notifications(q = {}) {
    return unwrap(await api.GET("/api/v1/notifications", { params: { query: q } }));
  },
  async unreadCount() {
    return unwrap(await api.GET("/api/v1/notifications/unread-count")).unread;
  },
  async markRead(id) {
    noContent(await api.POST("/api/v1/notifications/{id}/read", { params: { path: { id } } }));
  },
  async markAllRead() {
    return unwrap(await api.POST("/api/v1/notifications/read-all")).marked;
  },
  async notices(view = "current") {
    return unwrap(await api.GET("/api/v1/notices", { params: { query: { view } } }));
  },
  async createNotice(body) {
    return unwrap(await api.POST("/api/v1/notices", { body, params: { header: { "Idempotency-Key": newIdempotencyKey() } } }));
  },
  async archiveNotice(id) {
    return unwrap(await api.POST("/api/v1/notices/{id}/archive", { params: { path: { id } } }));
  },
  async todos(status = "open") {
    return unwrap(await api.GET("/api/v1/todos", { params: { query: { status } } }));
  },
  async createTodo(body) {
    return unwrap(await api.POST("/api/v1/todos", { body, params: { header: { "Idempotency-Key": newIdempotencyKey() } } }));
  },
  async setTodo(id, action) {
    const params = { params: { path: { id } } };
    if (action === "complete") return unwrap(await api.POST("/api/v1/todos/{id}/complete", params));
    if (action === "dismiss") return unwrap(await api.POST("/api/v1/todos/{id}/dismiss", params));
    return unwrap(await api.POST("/api/v1/todos/{id}/reopen", params));
  },
  async profile() {
    return unwrap(await api.GET("/api/v1/profile"));
  },
  async updateProfile(version, body) {
    return unwrap(await api.PATCH("/api/v1/profile", { body, params: { header: { "If-Match": `"${version}"` } } }));
  },
  async sessions() {
    return unwrap(await api.GET("/api/v1/auth/sessions")).items;
  },
  async revokeSession(id) {
    noContent(await api.DELETE("/api/v1/auth/sessions/{id}", { params: { path: { id } } }));
  },
  async revokeOthers() {
    return unwrap(await api.POST("/api/v1/auth/sessions/revoke-others")).revoked;
  },
  async mfa() {
    return unwrap(await api.GET("/api/v1/auth/mfa"));
  },
};

// ---------------------------------------------------------------------------------------------
// Mock mode
// ---------------------------------------------------------------------------------------------

const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();
let seq = 0;
const id = () => `00000000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;

/** In-memory inbox for `mock` mode (and tests); state lives as long as the returned object. */
export function mockInbox(role: Role): InboxApi {
  const cfg = ROLE_CONFIG[role];
  let notifications: Notification[] = cfg.notifications.map((n, i) => ({
    id: id(),
    category: i === cfg.notifications.length - 1 ? "notice" : "system",
    severity: "info",
    title: n.title,
    body: n.meta,
    link: null,
    createdAt: ago(i * 90 + 5),
    readAt: null,
  }));
  let notices: Notice[] = [
    {
      id: id(),
      title: ANNOUNCEMENT.title,
      body: ANNOUNCEMENT.meta,
      audienceRoles: ["administrator", "faculty", "student"],
      pinned: true,
      publishAt: ago(600),
      expiresAt: null,
      state: "live",
      authorName: "Student affairs",
      version: 1,
      createdAt: ago(600),
      updatedAt: ago(600),
    },
  ];
  let todos: Todo[] = cfg.todos.map((t, i) => ({
    id: id(),
    title: t.title,
    detail: t.meta,
    dueAt: null,
    link: null,
    status: "open",
    source: "manual",
    assignedBy: null,
    overdue: t.status === "attention",
    version: 1,
    createdAt: ago(i * 60),
    updatedAt: ago(i * 60),
    completedAt: null,
    dismissedAt: null,
  }));
  let profile: Profile = {
    subject: `dev|${cfg.loginId}`,
    loginId: cfg.loginId,
    displayName: cfg.name,
    roles: [role],
    locale: "en-IN",
    timeZone: "Asia/Kolkata",
    contactEmail: null,
    contactPhone: null,
    notificationPreferences: (["institution", "notice", "todo", "security", "system"] as const).map((category) => ({
      category,
      inApp: true,
      email: category === "security",
    })),
    version: 1,
    updatedAt: ago(1),
    lastSignInAt: ago(1),
  };
  let sessions: SessionView[] = [
    { id: id(), createdAt: ago(3), lastSeenAt: ago(0), expiresAt: ago(-480), idleExpiresAt: ago(-30), mfaLevel: "none", amr: ["pwd"], federated: false, current: true },
    { id: id(), createdAt: ago(1440), lastSeenAt: ago(60), expiresAt: ago(-60), idleExpiresAt: ago(-10), mfaLevel: "none", amr: ["pwd"], federated: false, current: false },
  ];
  const visibleNotices = () => notices.filter((n) => n.state === "live" && n.audienceRoles.includes(role));
  const unread = () => notifications.filter((n) => !n.readAt).length;
  const todoPage = (status: TodoFilter): TodoPage => ({
    items: todos.filter((t) => status === "all" || t.status === status),
    nextCursor: null,
    openCount: todos.filter((t) => t.status === "open").length,
  });
  return {
    async notifications(q = {}) {
      const items = notifications.filter((n) => !q.unreadOnly || !n.readAt);
      return { items, nextCursor: null, unreadCount: unread() };
    },
    async unreadCount() {
      return unread();
    },
    async markRead(nid) {
      notifications = notifications.map((n) => (n.id === nid && !n.readAt ? { ...n, readAt: new Date().toISOString() } : n));
    },
    async markAllRead() {
      const n = unread();
      notifications = notifications.map((x) => (x.readAt ? x : { ...x, readAt: new Date().toISOString() }));
      return n;
    },
    async notices(view = "current") {
      const items = view === "manage" ? notices : visibleNotices();
      return { items, total: items.length };
    },
    async createNotice(n) {
      const now = new Date().toISOString();
      const notice: Notice = {
        id: id(),
        title: n.title,
        body: n.body,
        audienceRoles: n.audienceRoles,
        pinned: n.pinned ?? false,
        publishAt: n.publishAt ?? now,
        expiresAt: n.expiresAt ?? null,
        state: n.publishAt && n.publishAt > now ? "scheduled" : "live",
        authorName: cfg.name,
        version: 1,
        createdAt: now,
        updatedAt: now,
      };
      notices = [notice, ...notices];
      return notice;
    },
    async archiveNotice(nid) {
      notices = notices.map((n) => (n.id === nid ? { ...n, state: "archived", version: n.version + 1 } : n));
      const found = notices.find((n) => n.id === nid);
      if (!found) throw new Error("Notice not found");
      return found;
    },
    async todos(status = "open") {
      return todoPage(status);
    },
    async createTodo(t) {
      const now = new Date().toISOString();
      const todo: Todo = {
        id: id(),
        title: t.title,
        detail: t.detail ?? "",
        dueAt: t.dueAt ?? null,
        link: t.link ?? null,
        status: "open",
        source: "manual",
        assignedBy: null,
        overdue: false,
        version: 1,
        createdAt: now,
        updatedAt: now,
        completedAt: null,
        dismissedAt: null,
      };
      if (!t.assigneeLoginId) todos = [todo, ...todos];
      return todo;
    },
    async setTodo(tid, action) {
      const now = new Date().toISOString();
      todos = todos.map((t) =>
        t.id !== tid
          ? t
          : action === "complete"
            ? { ...t, status: "completed", completedAt: now, dismissedAt: null }
            : action === "dismiss"
              ? { ...t, status: "dismissed", dismissedAt: now, completedAt: null }
              : { ...t, status: "open", completedAt: null, dismissedAt: null },
      );
      const found = todos.find((t) => t.id === tid);
      if (!found) throw new Error("To-do not found");
      return found;
    },
    async profile() {
      return profile;
    },
    async updateProfile(version, patch) {
      if (version !== profile.version) throw new Error("Someone else changed your profile — reload and try again.");
      const prefs = patch.notificationPreferences
        ? profile.notificationPreferences.map((p) => patch.notificationPreferences?.find((x) => x.category === p.category) ?? p)
        : profile.notificationPreferences;
      profile = {
        ...profile,
        displayName: patch.displayName ?? profile.displayName,
        locale: patch.locale ?? profile.locale,
        timeZone: patch.timeZone ?? profile.timeZone,
        contactEmail: patch.contactEmail === undefined ? profile.contactEmail : patch.contactEmail || null,
        contactPhone: patch.contactPhone === undefined ? profile.contactPhone : patch.contactPhone || null,
        notificationPreferences: prefs,
        version: profile.version + 1,
        updatedAt: new Date().toISOString(),
      };
      return profile;
    },
    async sessions() {
      return sessions;
    },
    async revokeSession(sid) {
      sessions = sessions.filter((s) => s.id !== sid || s.current);
    },
    async revokeOthers() {
      const n = sessions.filter((s) => !s.current).length;
      sessions = sessions.filter((s) => s.current);
      return n;
    },
    async mfa() {
      return { managedBy: "sutra", totpEnrolled: false, passkeys: [], webauthnAvailable: false, mfaRequired: false, sessionMfaLevel: "none" };
    },
  };
}

const mocks = new Map<Role, InboxApi>();

/** The inbox implementation for the current mode and role. */
export function useInbox(): InboxApi {
  const { role } = useSession();
  return useMemo(() => {
    if (USES_API) return apiInbox;
    let m = mocks.get(role);
    if (!m) {
      m = mockInbox(role);
      mocks.set(role, m);
    }
    return m;
  }, [role]);
}

/** Query keys (role-scoped so a role switch never shows another persona's data). */
export const inboxKeys = {
  all: (role: Role) => ["inbox", role] as const,
  notifications: (role: Role, unreadOnly: boolean) => ["inbox", role, "notifications", unreadOnly] as const,
  unread: (role: Role) => ["inbox", role, "unread"] as const,
  notices: (role: Role, view: string) => ["inbox", role, "notices", view] as const,
  todos: (role: Role, status: TodoFilter) => ["inbox", role, "todos", status] as const,
  profile: (role: Role) => ["inbox", role, "profile"] as const,
  sessions: (role: Role) => ["inbox", role, "sessions"] as const,
  mfa: (role: Role) => ["inbox", role, "mfa"] as const,
};

/** Unread notifications for the bell (refreshed every minute). */
export function useUnreadCount() {
  const inbox = useInbox();
  const { role } = useSession();
  return useQuery({ queryKey: inboxKeys.unread(role), queryFn: () => inbox.unreadCount(), refetchInterval: 60_000 });
}

/** Invalidates every inbox query of the current role after a mutation. */
export function useInboxRefresh() {
  const qc = useQueryClient();
  const { role } = useSession();
  return () => qc.invalidateQueries({ queryKey: inboxKeys.all(role) });
}

/** Relative time such as "5 min ago" / "in 2 h". */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const diff = Math.round((new Date(iso).getTime() - now) / 60_000);
  const abs = Math.abs(diff);
  const fmt = abs < 1 ? "just now" : abs < 60 ? `${abs} min` : abs < 1440 ? `${Math.round(abs / 60)} h` : `${Math.round(abs / 1440)} d`;
  if (fmt === "just now") return fmt;
  return diff < 0 ? `${fmt} ago` : `in ${fmt}`;
}
