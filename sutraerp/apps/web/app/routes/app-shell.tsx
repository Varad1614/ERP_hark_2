import { Bell, BellOff, Check, KeyRound, ListTodo, LogOut, Megaphone, Search, ShieldCheck, User } from "lucide-react";
import type { Notification } from "@sutra/api-client";
import {
  Avatar,
  CommandPalette,
  IconButton,
  Popover,
  ROLE_ACCENTS,
  RoleTheme,
  Shell,
  useToast,
  type NavLinkLikeProps,
  type PaletteItem,
} from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Navigate, NavLink, Outlet, useNavigate } from "react-router";
import { AUTH_MODE, useWorkspaces } from "../lib/api";
import { useAuth, type SignInResult } from "../lib/auth";
import { NotificationRow } from "../components/NotificationRow";
import { groupMeta } from "../lib/icons";
import { inboxKeys, useInbox, useInboxRefresh, useUnreadCount } from "../lib/inbox";
import { ROLE_CONFIG, ROLES, routeFor, type Role } from "../lib/roles";

function RouterLink({ to, className, end, children }: NavLinkLikeProps) {
  return (
    <NavLink to={to} end={end} className={className}>
      {children}
    </NavLink>
  );
}

export default function AppShell() {
  const { ready, session, signIn, signOut } = useAuth();
  if (!ready) return null;
  if (!session) return <Navigate to="/sign-in" replace />;
  return <AuthenticatedShell role={session.role} signIn={signIn} signOut={signOut} />;
}

function AuthenticatedShell({
  role,
  signIn,
  signOut,
}: {
  role: Role;
  signIn: (r: Role) => Promise<SignInResult>;
  signOut: () => Promise<void>;
}) {
  const cfg = ROLE_CONFIG[role];
  const navigate = useNavigate();
  const toast = useToast();
  const [pop, setPop] = useState<"bell" | "menu" | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const inbox = useInbox();
  const refreshInbox = useInboxRefresh();
  const unreadQuery = useUnreadCount();
  // Latest notifications for the bell; fetched when the popover opens (and kept for a minute).
  const recent = useQuery({
    queryKey: [...inboxKeys.notifications(role, false), "bell"],
    queryFn: () => inbox.notifications({ limit: 6 }),
    enabled: pop === "bell",
    staleTime: 60_000,
  });
  const workspaces = useWorkspaces(role);

  useEffect(() => setPop(null), [role]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((o) => !o);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const unread = unreadQuery.data ?? 0;

  const openNotification = async (n: Notification) => {
    if (!n.readAt) {
      await inbox.markRead(n.id).catch(() => undefined);
      await refreshInbox();
    }
    if (n.link) {
      setPop(null);
      navigate(n.link);
    }
  };
  const markAllRead = async () => {
    await inbox.markAllRead().catch(() => 0);
    await refreshInbox();
  };
  const nav = cfg.nav.map((n) => ({ id: n.id, label: n.label, to: routeFor(n.id) }));

  const paletteItems = useMemo<PaletteItem[]>(
    () => [
      ...cfg.nav.map((n) => ({ id: `screen:${n.id}`, title: n.label, subtitle: "Screen", group: "Screens", icon: n.icon, tone: "grey" as const })),
      ...(workspaces.data?.items ?? []).map((w) => {
        const g = groupMeta(w.group);
        return { id: `ws:${w.key}`, title: w.name, subtitle: `${w.key} · Workspace`, group: "Workspaces", icon: g.icon, tone: g.tone };
      }),
    ],
    [cfg.nav, workspaces.data],
  );

  const onPick = useCallback(
    (item: PaletteItem) => {
      const [kind, id] = item.id.split(":");
      navigate(kind === "ws" ? `/workspaces/${id}` : routeFor(id ?? "home"));
    },
    [navigate],
  );

  const switchRole = async (next: Role) => {
    setPop(null);
    if (next === role) return;
    try {
      const result = await signIn(next);
      if (result.status === "mfaRequired") {
        toast(`${ROLE_CONFIG[next].name} needs two-step verification — sign out and sign in again.`);
        return;
      }
      toast(`Signed in as ${ROLE_CONFIG[next].name}`);
      navigate("/");
    } catch (e) {
      toast(e instanceof Error ? e.message : "Could not switch role");
    }
  };

  const actions = (
    <>
      <IconButton label="Search (Ctrl K)" onClick={() => setPaletteOpen(true)}>
        <Search strokeWidth={1.75} />
      </IconButton>
      <span data-popover-trigger>
        <IconButton label={`Notifications, ${unread} unread`} dot={unread > 0} onClick={() => setPop((p) => (p === "bell" ? null : "bell"))}>
          <Bell strokeWidth={1.75} />
        </IconButton>
      </span>
      <span data-popover-trigger>
        <Avatar initials={cfg.initials} label="Account menu" onClick={() => setPop((p) => (p === "menu" ? null : "menu"))} />
      </span>
      <Popover open={pop === "bell"} onClose={() => setPop(null)} label="Notifications">
        <h5>
          Notifications
          <button type="button" onClick={markAllRead} disabled={unread === 0} data-bell-read-all>
            Mark all read
          </button>
        </h5>
        {recent.isLoading ? (
          <div className="sb-skeleton" style={{ width: "70%" }} />
        ) : recent.isError ? (
          <p className="sb-pop-label">Notifications are unavailable right now.</p>
        ) : (recent.data?.items.length ?? 0) === 0 ? (
          <div className="sb-pop-item is-read">
            <BellOff strokeWidth={1.75} />
            <div>
              <b>You’re all caught up</b>
            </div>
          </div>
        ) : (
          recent.data?.items.map((n) => <NotificationRow key={n.id} n={n} onOpen={openNotification} />)
        )}
        <div className="sb-pop-sep" />
        <button type="button" className="sb-pop-item" data-bell-see-all onClick={() => { setPop(null); navigate("/notifications"); }}>
          <Bell strokeWidth={1.75} />
          <div><b>See all notifications</b></div>
        </button>
      </Popover>
      <Popover open={pop === "menu"} onClose={() => setPop(null)} variant="menu" label="Account menu">
        <div className="sb-pop-item">
          <span className="sb-avatar" style={{ width: 36, height: 36, fontSize: 13 }}>
            {cfg.initials}
          </span>
          <div>
            <b>{cfg.name}</b>
            <small>{cfg.title} · Sutra Institute of Technology</small>
          </div>
        </div>
        <button type="button" className="sb-pop-item" onClick={() => { setPop(null); navigate("/profile"); }}>
          <User strokeWidth={1.75} />
          <div><b>My profile</b></div>
        </button>
        <button type="button" className="sb-pop-item" onClick={() => { setPop(null); navigate("/notices"); }}>
          <Megaphone strokeWidth={1.75} />
          <div><b>Notices</b></div>
        </button>
        <button type="button" className="sb-pop-item" data-todos-menu onClick={() => { setPop(null); navigate("/todos"); }}>
          <ListTodo strokeWidth={1.75} />
          <div><b>To-dos</b></div>
        </button>
        <button type="button" className="sb-pop-item" data-security-menu onClick={() => { setPop(null); navigate("/account/security"); }}>
          <KeyRound strokeWidth={1.75} />
          <div><b>Security</b><small>Sessions, passkeys</small></div>
        </button>
        <button type="button" className="sb-pop-item" data-mfa-menu onClick={() => { setPop(null); navigate("/account/mfa"); }}>
          <ShieldCheck strokeWidth={1.75} />
          <div><b>Two-step verification</b></div>
        </button>
        <div className="sb-pop-sep" />
        <p className="sb-pop-label">Switch role</p>
        {ROLES.map((r) => {
          const x = ROLE_CONFIG[r];
          return (
            <button key={r} type="button" className="sb-pop-item" data-switch-role={r} onClick={() => switchRole(r)}>
              <span className="sb-avatar" data-role-swatch={r} style={{ width: 30, height: 30, fontSize: 11, background: ROLE_ACCENTS[r].acs, color: ROLE_ACCENTS[r].ac }}>
                {x.initials}
              </span>
              <div>
                <b>{x.title}</b>
                <small>{x.name}</small>
              </div>
              {r === role && <Check strokeWidth={1.75} />}
            </button>
          );
        })}
        <div className="sb-pop-sep" />
        <button
          type="button"
          className="sb-pop-item"
          onClick={async () => {
            setPop(null);
            await signOut();
            navigate("/sign-in", { replace: true });
          }}
        >
          <LogOut strokeWidth={1.75} />
          <div><b>Sign out</b></div>
        </button>
      </Popover>
    </>
  );

  return (
    <>
      <RoleTheme role={role} />
      <Shell
        nav={nav}
        linkComponent={RouterLink}
        actions={actions}
        footer={
          <div className="sb-env-tag">
            <i />
            {AUTH_MODE === "mock" ? "Batch 1 · mock data" : AUTH_MODE === "oidc" ? "Batch 1 · live API · SSO" : "Batch 1 · live API"}
          </div>
        }
      >
        <Outlet />
      </Shell>
      <CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} items={paletteItems} onSelect={onPick} />
    </>
  );
}
