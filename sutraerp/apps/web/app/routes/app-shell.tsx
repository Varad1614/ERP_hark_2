import { Bell, Check, LogOut, Megaphone, Search, ShieldCheck, User } from "lucide-react";
import {
  Avatar,
  CommandPalette,
  IconButton,
  IconChip,
  Popover,
  ROLE_ACCENTS,
  RoleTheme,
  Shell,
  useToast,
  type NavLinkLikeProps,
  type PaletteItem,
} from "@sutra/ui";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Navigate, NavLink, Outlet, useNavigate } from "react-router";
import { AUTH_MODE, useWorkspaces } from "../lib/api";
import { useAuth, type SignInResult } from "../lib/auth";
import { groupMeta } from "../lib/icons";
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
  const [read, setRead] = useState<Record<string, boolean>>({});
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

  const unread = cfg.notifications.filter((_, i) => !read[`${role}${i}`]).length;
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
          <button type="button" onClick={() => setRead(Object.fromEntries(cfg.notifications.map((_, i) => [`${role}${i}`, true])))}>
            Mark all read
          </button>
        </h5>
        {cfg.notifications.map((n, i) => (
          <button key={n.title} type="button" className={`sb-pop-item${read[`${role}${i}`] ? " is-read" : ""}`} onClick={() => setRead((r) => ({ ...r, [`${role}${i}`]: true }))}>
            <IconChip icon={n.icon} tone={n.tone} size="sm" />
            <div>
              <b>{n.title}</b>
              <small>{n.meta}</small>
            </div>
            <span className="sb-unread" />
          </button>
        ))}
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
