/**
 * App shell (re-skin v2, prototype header): logo, pill navigation, search (Ctrl K palette),
 * notifications bell and the avatar menu with its collapsible Personalization group (Customize
 * Home, Theme, Layouts, Focus mode, Print) + Install as an app, Take the tour, Institution
 * branding (administrator), Data import (holders of `platform.import.manage`; Import approvals for
 * holders of `platform.import.approve` only — Part 2 · S4-ui-import), Switch role and Sign out. The personal theme applies on every page.
 *
 * Part 2 · S7-entitle: pills, the mobile tabs, the palette's screens and workspaces show active
 * modules only; a route of an inactive module renders a "not active" state instead of the screen;
 * the licence banner (grace / expired / suspended / renewal due) sits above every page. The API
 * enforces all of it (`403 entitlement.*`).
 */
import {
  Bell,
  BellOff,
  Building2,
  Check,
  ChevronDown,
  Contrast,
  FileSpreadsheet,
  Focus,
  KeyRound,
  LayoutGrid,
  Lock,
  LayoutTemplate,
  ListTodo,
  LogOut,
  Map as MapIcon,
  Megaphone,
  Menu,
  MonitorSmartphone,
  Moon,
  Printer,
  Search,
  ShieldCheck,
  SlidersHorizontal,
  Smartphone,
  Sun,
  SwatchBook,
  Type,
  User,
} from "lucide-react";
import type { Notification } from "@sutra/api-client";
import { Avatar, Drawer, EmptyState, IconButton, IconChip, Popover, ROLE_ACCENTS, RoleTheme, Shell, StepDialog, useToast, type NavLinkLikeProps } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Navigate, NavLink, Outlet, useLocation, useNavigate } from "react-router";
import { AUTH_MODE, useWorkspaces } from "../lib/api";
import { useAuth, type SignInCredentials, type SignInResult } from "../lib/auth";
import { can } from "../lib/permissions";
import { LicenceBanner } from "../components/LicenceBanner";
import { NotificationRow } from "../components/NotificationRow";
import { isModuleActive, licenceBanner, moduleForRoute, moduleName, useEntitlement, visibleNav, visibleWorkspaces } from "../lib/entitlements";
import { Palette, type PaletteEntry } from "../components/Palette";
import { groupMeta } from "../lib/icons";
import { inboxKeys, useInbox, useInboxRefresh, useUnreadCount } from "../lib/inbox";
import { devPersonasEnabled } from "../lib/dev-personas";
import { ROLE_CONFIG, ROLES, routeFor, type Role } from "../lib/roles";
import { ThemeDrawer } from "../features/home/Customize";
import { PRESETS, presetLayout } from "../features/home/model";
import { effectiveLayout, homeUi, setLayout, setTheme, themeSnapshot, useHomeUi, useTheme, type ThemeMode } from "../features/home/store";
import { useApplyTheme } from "../features/home/theme";
import "../components/shell.css";

function RouterLink({ to, className, end, match, children }: NavLinkLikeProps) {
  const { pathname } = useLocation();
  const alias = (match ?? []).some((m) => pathname === m || pathname.startsWith(`${m}/`));
  const cls = typeof className === "function" ? ({ isActive }: { isActive: boolean }) => className({ isActive: isActive || alias }) : className;
  return (
    <NavLink to={to} end={end} className={cls} aria-current={alias ? "page" : undefined}>
      {children}
    </NavLink>
  );
}

export default function AppShell() {
  const { ready, session, signIn, signOut } = useAuth();
  const location = useLocation();
  if (!ready) return null;
  if (!session) {
    // Come back to the requested page after sign-in (the sign-in page re-validates the path).
    const back = `${location.pathname}${location.search}`;
    return <Navigate to={back === "/" ? "/sign-in" : `/sign-in?returnTo=${encodeURIComponent(back)}`} replace />;
  }
  return <AuthenticatedShell role={session.role} signIn={signIn} signOut={signOut} />;
}

interface InstallPrompt extends Event {
  prompt: () => Promise<void>;
}

const TOUR = [
  ["Your Home", "The tiles show what needs you today. Open Personalization in your avatar menu to rearrange them."],
  ["Navigation", "The pills across the top take you to every area of the ERP for your role."],
  ["Search and commands", "Press Ctrl K (⌘K on a Mac) to jump to any screen or workspace, or run a command like Dark mode."],
  ["Notifications", "The bell collects what changed for you. Open one to go straight to where the work happens."],
] as const;

function isTyping(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null;
  return !!t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
}

function AuthenticatedShell({ role, signIn, signOut }: { role: Role; signIn: (c: SignInCredentials) => Promise<SignInResult>; signOut: () => Promise<void> }) {
  const cfg = ROLE_CONFIG[role];
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const toast = useToast();
  const [pop, setPop] = useState<"bell" | "menu" | null>(null);
  const [pzOpen, setPzOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [tour, setTour] = useState<number | null>(null);
  const [moreOpen, setMoreOpen] = useState(false);
  const [install, setInstall] = useState<InstallPrompt | null>(null);
  const inbox = useInbox();
  const refreshInbox = useInboxRefresh();
  const unreadQuery = useUnreadCount();
  const { panel, focus } = useHomeUi();
  const theme = useTheme();
  useApplyTheme();
  // Latest notifications for the bell; fetched when the popover opens (and kept for a minute).
  const recent = useQuery({
    queryKey: [...inboxKeys.notifications(role, false), "bell"],
    queryFn: () => inbox.notifications({ limit: 6 }),
    enabled: pop === "bell",
    staleTime: 60_000,
  });
  const workspaces = useWorkspaces(role);
  // Part 2 · S7-entitle: active modules + licence state of the institution.
  const entitlement = useEntitlement();
  const ent = entitlement.data;
  const navEntries = useMemo(() => visibleNav(cfg.nav, ent), [cfg.nav, ent]);
  const banner = licenceBanner(ent);
  const routeModule = moduleForRoute(pathname);
  const routeBlocked = !isModuleActive(ent, routeModule);
  const onHome = pathname === "/";
  // Part 2 · S4-ui-import: the Data import entry follows the session's permissions (the API enforces them).
  const { session } = useAuth();
  const importEntry = useMemo(
    () =>
      can(session, "platform.import.manage")
        ? { to: "/data-import", label: "Data import", sub: "CSV / Excel migration" }
        : can(session, "platform.import.approve")
          ? { to: "/data-import/approvals", label: "Import approvals", sub: "Imports waiting for you" }
          : null,
    [session],
  );

  useEffect(() => setPop(null), [role]);
  useEffect(() => {
    const onPrompt = (e: Event) => {
      e.preventDefault();
      setInstall(e as InstallPrompt);
    };
    window.addEventListener("beforeinstallprompt", onPrompt);
    return () => window.removeEventListener("beforeinstallprompt", onPrompt);
  }, []);

  const goHomeThen = useCallback(
    (fn: () => void) => {
      if (!onHome) navigate("/");
      // Let Home mount first when we navigated.
      setTimeout(fn, onHome ? 0 : 60);
    },
    [onHome, navigate],
  );
  const customize = useCallback(() => goHomeThen(() => homeUi.startCustomize()), [goHomeThen]);
  const toggleFocus = useCallback(() => {
    if (homeUi.get().focus) homeUi.setFocus(false);
    else goHomeThen(() => homeUi.setFocus(true));
  }, [goHomeThen]);
  const openLayouts = useCallback(() => goHomeThen(() => homeUi.openPanel("layouts")), [goHomeThen]);
  const print = useCallback(() => setTimeout(() => window.print(), 50), []);
  const installApp = useCallback(async () => {
    if (install) {
      await install.prompt().catch(() => undefined);
      setInstall(null);
    } else toast("To install SutraERP, use your browser’s Install app option (address bar or menu).");
  }, [install, toast]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((o) => !o);
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey || isTyping(e) || document.querySelector(".sb-overlay")) return;
      const k = e.key.toLowerCase();
      if (k === "e" && onHome) {
        e.preventDefault();
        if (homeUi.get().editing) homeUi.stopCustomize();
        else customize();
      } else if (k === "f" && onHome) {
        e.preventDefault();
        toggleFocus();
      } else if (k === "d") {
        e.preventDefault();
        setTheme({ mode: themeSnapshot().mode === "dark" || themeSnapshot().mode === "oled" ? "light" : "dark" });
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onHome, customize, toggleFocus]);

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
  const nav = navEntries.map((n) => ({ id: n.id, label: n.label, to: routeFor(n.id), match: n.match }));

  const dark = theme.mode === "dark" || theme.mode === "oled";
  const paletteItems = useMemo<PaletteEntry[]>(() => {
    const act = (id: string, title: string, subtitle: string, icon: PaletteEntry["icon"], tone: PaletteEntry["tone"], keywords = "", kbd?: string): PaletteEntry => ({ id: `cmd:${id}`, title, subtitle, group: "Actions", icon, tone, keywords, ...(kbd ? { kbd } : {}) });
    return [
      act("customize", "Customize Home", "Personalization", LayoutGrid, "orange", "edit widgets tiles drag", "E"),
      act("focus", focus ? "Exit focus mode" : "Focus mode", "Just your queue", Focus, "grey", "focus quiet", "F"),
      act("theme", "Theme settings", "Accent, background, corners, font", SwatchBook, "violet", "colour accent font appearance"),
      act("layouts", "Layouts", "Switch your Home for the season", LayoutTemplate, "teal", "preset season"),
      ...PRESETS[role].map(([key, name, desc]) => act(`preset:${key}`, `Switch layout: ${name}`, desc, LayoutTemplate, "teal", "layout preset")),
      act(dark ? "light" : "dark", dark ? "Light mode" : "Dark mode", "Theme", dark ? Sun : Moon, "grey", "theme appearance", "D"),
      act("auto", "Match device theme", "Theme", MonitorSmartphone, "grey", "auto system"),
      act("oled", "OLED black mode", "Theme", Contrast, "grey", "black night"),
      act("contrast", theme.hc ? "Standard contrast" : "High contrast", "Accessibility", Contrast, "grey", "a11y accessibility"),
      act("font", theme.font === "easy" ? "Standard font" : "Easy-read font", "Accessibility · Atkinson Hyperlegible", Type, "grey", "dyslexia accessibility"),
      act("print", "Print or save as PDF", "This page", Printer, "grey", "pdf export"),
      act("todos", "To-dos", "Your work queue", ListTodo, "orange", "tasks queue"),
      act("notices", "Notices", "Announcements for your role", Megaphone, "green", "announcements"),
      act("notifications", "Notifications", "Everything that changed for you", Bell, "violet", "inbox alerts"),
      act("profile", "My profile", "Details and preferences", User, "grey", "account"),
      act("security", "Security", "Sessions, passkeys", KeyRound, "grey", "sessions passkeys password"),
      act("mfa", "Two-step verification", "Authenticator app", ShieldCheck, "grey", "mfa totp 2fa"),
      ...(role === "administrator" ? [act("branding", "Institution branding", "Name, logo and brand colour", Building2, "teal", "logo brand")] : []),
      ...(importEntry ? [act("data-import", importEntry.label, importEntry.sub, FileSpreadsheet, "blue", "import csv excel xlsx migration upload export")] : []),
      ...navEntries.map((n) => ({ id: `screen:${n.id}`, title: n.label, subtitle: "Screen", group: "Screens", icon: n.icon, tone: "grey" as const })),
      ...visibleWorkspaces(workspaces.data?.items ?? [], ent).map((w) => {
        const g = groupMeta(w.group);
        return { id: `ws:${w.key}`, title: w.name, subtitle: `${w.key} · Workspace`, group: "Workspaces", icon: g.icon, tone: g.tone };
      }),
    ];
  }, [navEntries, workspaces.data, ent, role, focus, dark, theme.hc, theme.font, importEntry]);
  const suggested = useMemo(() => paletteItems.filter((x) => ["cmd:customize", "cmd:todos", `cmd:${dark ? "light" : "dark"}`, "cmd:theme"].includes(x.id) || x.id === `screen:${navEntries[1]?.id}`), [paletteItems, dark, navEntries]);

  const onPick = useCallback(
    (item: PaletteEntry) => {
      const [kind, ...rest] = item.id.split(":");
      const id = rest.join(":");
      if (kind === "cmd") {
        const t = themeSnapshot();
        switch (id) {
          case "customize":
            return customize();
          case "focus":
            return toggleFocus();
          case "theme":
            return homeUi.openPanel("theme");
          case "layouts":
            return openLayouts();
          case "light":
          case "dark":
          case "auto":
          case "oled":
            setTheme({ mode: id as ThemeMode });
            return toast(`Theme: ${item.title}`);
          case "contrast":
            setTheme({ hc: !t.hc });
            return toast(`Display: ${item.title}`);
          case "font":
            setTheme({ font: t.font === "easy" ? "sutra" : "easy" });
            return toast(`Display: ${item.title}`);
          case "print":
            return print();
          case "todos":
          case "notices":
          case "notifications":
          case "profile":
            return navigate(`/${id}`);
          case "security":
            return navigate("/account/security");
          case "mfa":
            return navigate("/account/mfa");
          case "branding":
            return navigate("/campus/institution");
          case "data-import":
            return navigate(importEntry?.to ?? "/data-import");
          default:
            if (id.startsWith("preset:")) {
              const key = id.slice(7);
              goHomeThen(() => {
                setLayout(presetLayout(key, effectiveLayout(), role));
                toast(item.title.replace("Switch layout: ", "Layout: "));
              });
            }
            return;
        }
      }
      navigate(kind === "ws" ? `/workspaces/${id}` : routeFor(id || "home"));
    },
    [navigate, toast, customize, toggleFocus, openLayouts, print, goHomeThen, role, importEntry],
  );

  const switchRole = async (next: Role) => {
    setPop(null);
    if (next === role) return;
    // API mode: a persona needs the typed dev password, so switching means signing in again.
    if (AUTH_MODE !== "mock") {
      await signOut();
      toast(`Sign in as ${ROLE_CONFIG[next].name} with the development password`);
      navigate("/sign-in", { replace: true });
      return;
    }
    try {
      const result = await signIn({ role: next });
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

  const menuGo = (fn: () => void) => () => {
    setPop(null);
    fn();
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
        <button type="button" className="bell-all" data-bell-see-all onClick={menuGo(() => navigate("/notifications"))}>
          See all notifications
        </button>
      </Popover>
      <Popover open={pop === "menu"} onClose={() => setPop(null)} variant="menu" label="Account menu">
        <div className="sb-pop-item mn-me">
          <span className="sb-avatar" style={{ width: 36, height: 36, fontSize: 13 }}>
            {cfg.initials}
          </span>
          <div>
            <b>{cfg.name}</b>
            <small>{cfg.title} · Sutra Institute of Technology</small>
          </div>
        </div>
        <button type="button" className="sb-pop-item mn" data-profile-menu onClick={menuGo(() => navigate("/profile"))}>
          <User strokeWidth={1.75} />
          <div>
            <b>My profile</b>
          </div>
        </button>
        <button type="button" className="sb-pop-item mn" data-notices-menu onClick={menuGo(() => navigate("/notices"))}>
          <Megaphone strokeWidth={1.75} />
          <div>
            <b>Notices</b>
          </div>
        </button>
        <button type="button" className={`sb-pop-item mn mn-pz${pzOpen ? " is-open" : ""}`} aria-expanded={pzOpen} data-pz-toggle onClick={() => setPzOpen((o) => !o)}>
          <SlidersHorizontal strokeWidth={1.75} />
          <div>
            <b>Personalization</b>
            <small>Customize, theme, layouts, focus</small>
          </div>
          <ChevronDown strokeWidth={1.75} className="mn-chev" />
        </button>
        {pzOpen && (
          <div className="mn-sub" data-pz-group>
            <button type="button" className="sb-pop-item mn" data-pz="customize" onClick={menuGo(customize)}>
              <LayoutGrid strokeWidth={1.75} />
              <div>
                <b>Customize Home</b>
              </div>
              <kbd className="sb-kbd">E</kbd>
            </button>
            <button type="button" className="sb-pop-item mn" data-pz="theme" onClick={menuGo(() => homeUi.openPanel("theme"))}>
              <SwatchBook strokeWidth={1.75} />
              <div>
                <b>Theme</b>
              </div>
            </button>
            <button type="button" className="sb-pop-item mn" data-pz="layouts" onClick={menuGo(openLayouts)}>
              <LayoutTemplate strokeWidth={1.75} />
              <div>
                <b>Layouts</b>
              </div>
            </button>
            <button type="button" className="sb-pop-item mn" data-pz="focus" onClick={menuGo(toggleFocus)}>
              <Focus strokeWidth={1.75} />
              <div>
                <b>{focus ? "Exit focus mode" : "Focus mode"}</b>
              </div>
              <kbd className="sb-kbd">F</kbd>
            </button>
            <button type="button" className="sb-pop-item mn" data-pz="print" onClick={menuGo(print)}>
              <Printer strokeWidth={1.75} />
              <div>
                <b>Print or save as PDF</b>
              </div>
            </button>
          </div>
        )}
        <button type="button" className="sb-pop-item mn" data-install-menu onClick={menuGo(installApp)}>
          <Smartphone strokeWidth={1.75} />
          <div>
            <b>Install as an app</b>
          </div>
        </button>
        <button type="button" className="sb-pop-item mn" data-tour-menu onClick={menuGo(() => setTour(0))}>
          <MapIcon strokeWidth={1.75} />
          <div>
            <b>Take the tour</b>
          </div>
        </button>
        {role === "administrator" && (
          <button type="button" className="sb-pop-item mn" data-branding-menu onClick={menuGo(() => navigate("/campus/institution"))}>
            <Building2 strokeWidth={1.75} />
            <div>
              <b>Institution branding</b>
              <small>Name, logo, colour</small>
            </div>
          </button>
        )}
        {importEntry && (
          <button type="button" className="sb-pop-item mn" data-import-menu onClick={menuGo(() => navigate(importEntry.to))}>
            <FileSpreadsheet strokeWidth={1.75} />
            <div>
              <b>{importEntry.label}</b>
              <small>{importEntry.sub}</small>
            </div>
          </button>
        )}
        {/* Audit 2026-10 (SEC-2/M-3): the role switcher exists only in dev-persona builds, never with OIDC. */}
        {devPersonasEnabled() && AUTH_MODE !== "oidc" && <div className="sb-pop-sep" />}
        {devPersonasEnabled() && AUTH_MODE !== "oidc" && <p className="sb-pop-label">Switch role</p>}
        {(devPersonasEnabled() && AUTH_MODE !== "oidc" ? ROLES : []).map((r) => {
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
          className="sb-pop-item mn"
          data-sign-out
          onClick={async () => {
            setPop(null);
            await signOut();
            navigate("/sign-in", { replace: true });
          }}
        >
          <LogOut strokeWidth={1.75} />
          <div>
            <b>Sign out</b>
          </div>
        </button>
      </Popover>
    </>
  );

  const step = tour === null ? null : TOUR[tour];
  return (
    <>
      <RoleTheme role={role} />
      <Shell nav={nav} linkComponent={RouterLink} actions={actions} footer={AUTH_MODE === "mock" ? <div className="sb-env-tag"><i />Demo · mock data</div> : undefined}>
        {banner && <LicenceBanner model={banner} />}
        {routeBlocked && routeModule ? (
          <div data-module-inactive={routeModule}>
            <EmptyState icon={Lock} title={`${moduleName(routeModule)} is not active`}>
              This module is not part of your institution’s SutraERP plan. A platform administrator can activate it.
            </EmptyState>
          </div>
        ) : (
          <Outlet />
        )}
      </Shell>
      <nav className="dtab" aria-label="Main (mobile)" data-mobile-tabs>
        {navEntries.slice(0, 4).map((n) => (
          <RouterLink key={n.id} to={routeFor(n.id)} end={n.id === "home"} match={n.match} className={({ isActive }) => (isActive ? "on" : "")}>
            <n.icon strokeWidth={1.75} />
            {n.label}
          </RouterLink>
        ))}
        <button type="button" onClick={() => setMoreOpen(true)} data-mobile-more>
          <Menu strokeWidth={1.75} />
          More
        </button>
      </nav>
      <Drawer open={moreOpen} onClose={() => setMoreOpen(false)} title="More" eyebrow="Navigation">
        <div className="dh-presets">
          {navEntries.slice(4).map((n) => (
            <button
              key={n.id}
              type="button"
              className="sb-row"
              onClick={() => {
                setMoreOpen(false);
                navigate(routeFor(n.id));
              }}
            >
              <IconChip icon={n.icon} tone="grey" size="sm" />
              <div>
                <b>{n.label}</b>
              </div>
            </button>
          ))}
        </div>
      </Drawer>
      <Palette open={paletteOpen} onClose={() => setPaletteOpen(false)} items={paletteItems} suggested={suggested} onSelect={onPick} />
      <ThemeDrawer open={panel === "theme"} onClose={homeUi.closePanel} />
      {step && tour !== null && (
        <StepDialog
          open
          onClose={() => setTour(null)}
          eyebrow={`Tour · ${tour + 1} of ${TOUR.length}`}
          title={step[0]}
          steps={TOUR.map((s) => s[0])}
          step={tour}
          cancelLabel="Skip tour"
          primary={{ label: tour === TOUR.length - 1 ? "Done" : "Next →", onClick: () => setTour(tour === TOUR.length - 1 ? null : tour + 1) }}
          onBack={tour > 0 ? () => setTour(tour - 1) : undefined}
          data-tour=""
        >
          <p className="tour-copy">{step[1]}</p>
        </StepDialog>
      )}
    </>
  );
}
