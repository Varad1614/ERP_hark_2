/**
 * Home widgets (re-skin v2). Each renders one tile of the customizable Home from the APIs that
 * exist today (mock APIs in mock mode). Figures without a backend never get invented numbers:
 * a tile without data shows an honest empty line instead.
 */
import {
  ArrowUpRight,
  BadgeCheck,
  Bell,
  BellOff,
  Check,
  ChevronRight,
  CircleCheck,
  ListTodo,
  Megaphone,
  Sun,
  Sunrise,
  TriangleAlert,
  Wallet,
  Wrench,
  type LucideIcon,
} from "lucide-react";
import type { Todo as LiveTodo } from "@sutra/api-client";
import { Badge, BentoTile, Button, EmptyState, IconChip, SidePanel, formatNumber, formatWeekdayDate, useToast, type Tone } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { useDeliveryApi, dkeys } from "../academic/delivery/hooks";
import { useLeaveShortcutLabel } from "../people/leave";
import { formatInr } from "../finance";
import { akeys, useAnalyticsApi } from "../reports/analytics/hooks";
import { useInstitutions } from "../institution/hooks";
import { USES_API, useWorkspaces } from "../../lib/api";
import { useSession } from "../../lib/auth";
import { groupMeta } from "../../lib/icons";
import { inboxKeys, relativeTime, useInbox, useInboxRefresh } from "../../lib/inbox";
import { BADGE_LABEL, ROLE_CONFIG, routeFor, type Role, type Todo } from "../../lib/roles";
import { AdminKpis, AnnouncementTile, FacultyKpis, RightNowTile, StudentKpis, metricValue } from "./live";
import type { HomeTile } from "./model";

export interface WidgetProps {
  tile: HomeTile;
  editing: boolean;
  /** Persist a tile edit (sticky-note text). */
  onTileText?: (id: string, text: string) => void;
}

/** How many open to-dos the to-dos tile shows (the rest are one click away on /todos). */
export const HOME_TODO_LIMIT = 4;

function today(): string {
  return new Intl.DateTimeFormat("en-IN", { weekday: "long", day: "numeric", month: "long" }).format(new Date());
}
const shortDue = (d: Date) => new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short" }).format(d);
const timeOfDay = (d: Date) => new Intl.DateTimeFormat("en-IN", { hour: "numeric", minute: "2-digit" }).format(d);
const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
const startOfWeek = (d: Date) => startOfDay(d) - ((d.getDay() + 6) % 7) * 86_400_000;

/** Due badge of a live to-do: overdue, due today, due tomorrow, or the date; none without a due date. */
export function dueBadge(t: Pick<LiveTodo, "dueAt" | "overdue">, now: Date = new Date()): { kind: "attention" | "review" | "neutral"; label: string } | null {
  if (!t.dueAt) return null;
  const due = new Date(t.dueAt);
  if (t.overdue || due.getTime() < now.getTime()) return { kind: "attention", label: "Overdue" };
  const days = Math.round((startOfDay(due) - startOfDay(now)) / 86_400_000);
  if (days === 0) return { kind: "attention", label: `Due ${timeOfDay(due)}` };
  if (days === 1) return { kind: "review", label: "Due tomorrow" };
  return { kind: "neutral", label: `Due ${shortDue(due)}` };
}

/** Progress ring of the to-dos tile (prototype `.dring`): closed today of today's total. */
export function Ring({ done, total }: { done: number; total: number }) {
  const r = 15;
  const c = 2 * Math.PI * r;
  const frac = total ? done / total : 0;
  return (
    <span className={`dring${total > 0 && done >= total ? " full" : ""}`} aria-label={`${done} of ${total} done`} data-ring={`${done}/${total}`}>
      <svg viewBox="0 0 34 34" aria-hidden="true">
        <circle className="tr" cx="17" cy="17" r={r} />
        <circle className="fg" cx="17" cy="17" r={r} strokeDasharray={c} strokeDashoffset={c * (1 - frac)} />
      </svg>
      <b>
        {done}/{total}
      </b>
    </span>
  );
}

/* ------------------------------------------------------------------ greeting, now, metrics */

export function HeroWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const cfg = ROLE_CONFIG[role];
  const navigate = useNavigate();
  const to = cfg.hero.to.startsWith("todo:") ? "/todos" : routeFor(cfg.hero.to);
  return (
    <BentoTile variant="accent" className="sb-hero">
      <span className="sb-chip">
        <Sun strokeWidth={1.75} /> {today()}
      </span>
      <h1>
        {tile.title ?? (
          <>
            Hi {cfg.first},
            <br />
            let’s make today easy.
          </>
        )}
      </h1>
      <p>{cfg.hero.sub}</p>
      <Button className="sb-mt" icon={<ArrowUpRight strokeWidth={1.75} />} onClick={() => navigate(to)}>
        {cfg.hero.cta}
      </Button>
    </BentoTile>
  );
}

export function NowWidget() {
  const { role } = useSession();
  return <RightNowTile role={role} />;
}

export function KpiWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const navigate = useNavigate();
  const go = (to: string) => navigate(to.startsWith("todo:") ? "/todos" : routeFor(to));
  const i = tile.i ?? 0;
  return role === "administrator" ? <AdminKpis go={go} only={i} /> : role === "faculty" ? <FacultyKpis go={go} only={i} /> : <StudentKpis go={go} only={i} />;
}

export function ShortcutsWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const navigate = useNavigate();
  const leaveLabel = useLeaveShortcutLabel(role === "faculty", "Apply leave");
  return (
    <BentoTile title={tile.title ?? "Shortcuts"}>
      <div className="sb-shortcuts">
        {ROLE_CONFIG[role].shortcuts.map((s) => (
          <button key={s.label} type="button" className="sb-shortcut" onClick={() => navigate(s.to.startsWith("todo:") ? "/todos" : routeFor(s.to))} data-shortcut={s.to}>
            <div className="sb-shortcut-icon">
              <s.icon strokeWidth={1.75} />
            </div>
            {s.to === "leave" ? leaveLabel : s.label}
          </button>
        ))}
      </div>
    </BentoTile>
  );
}

export function NoticeWidget() {
  return <AnnouncementTile />;
}

/* ------------------------------------------------------------------ to-dos */

function TodoHead({ title, done, total, onAll }: { title: string; done: number; total: number; onAll: () => void }) {
  return (
    <h4 className="dh-tasks-head">
      <span>
        <Ring done={done} total={total} />
        {title}
      </span>
      <button type="button" onClick={onAll} data-todos-see-all>
        See all
      </button>
    </h4>
  );
}

/** Live to-dos (API mode): open items, completion through `POST /todos/{id}/complete`. */
function LiveTasks({ tile, editing }: WidgetProps) {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: inboxKeys.todos(role, "open"), queryFn: () => inbox.todos("open") });
  const doneQ = useQuery({ queryKey: inboxKeys.todos(role, "completed"), queryFn: () => inbox.todos("completed") });
  const [completing, setCompleting] = useState<Record<string, boolean>>({});
  const [openId, setOpenId] = useState<string | null>(null);
  const open = (q.data?.items ?? []).filter((t) => t.status === "open").sort((a, b) => (a.dueAt ?? "9999").localeCompare(b.dueAt ?? "9999"));
  const items = open.slice(0, HOME_TODO_LIMIT);
  const openCount = q.data?.openCount ?? open.length;
  const more = Math.max(0, openCount - items.length);
  const todayStart = startOfDay(new Date());
  const doneToday = (doneQ.data?.items ?? []).filter((t) => t.completedAt && new Date(t.completedAt).getTime() >= todayStart).length;
  const state = q.isLoading ? "loading" : q.isError ? "error" : items.length === 0 ? "empty" : "ready";
  const current = open.find((t) => t.id === openId);

  const complete = async (t: LiveTodo) => {
    if (completing[t.id]) return;
    setCompleting((c) => ({ ...c, [t.id]: true }));
    try {
      await inbox.setTodo(t.id, "complete");
      toast(`Done: ${t.title}`);
      setOpenId(null);
      await refresh();
    } catch (e) {
      toast(e instanceof Error ? `Couldn’t complete “${t.title}”: ${e.message}` : "Could not update");
    } finally {
      setCompleting((c) => Object.fromEntries(Object.entries(c).filter(([id]) => id !== t.id)));
    }
  };

  return (
    <BentoTile className="dh-tasks">
      <TodoHead title={tile.title ?? "To-dos"} done={doneToday} total={doneToday + openCount} onAll={() => navigate("/todos")} />
      <div data-home-todos={state}>
        {state === "loading" ? (
          <>
            <div className="sb-skeleton" style={{ width: "70%" }} />
            <div className="sb-skeleton" style={{ width: "55%" }} />
            <div className="sb-skeleton" style={{ width: "62%" }} />
          </>
        ) : state === "error" ? (
          <EmptyState icon={ListTodo} title="To-dos are unavailable">
            <span role="alert">{q.error instanceof Error ? q.error.message : "Try again shortly."}</span>{" "}
            <Button size="sm" variant="ghost" onClick={() => q.refetch()} data-todos-retry>
              Retry
            </Button>
          </EmptyState>
        ) : state === "empty" ? (
          <EmptyState icon={CircleCheck} title="All clear for today">
            Everything in your queue is done.
          </EmptyState>
        ) : (
          <>
            {items.map((t) => {
              const badge = dueBadge(t);
              const busy = !!completing[t.id];
              return (
                <div key={t.id} className={`tk${busy ? " done" : ""}`} data-home-todo={t.id}>
                  <button
                    type="button"
                    className="box"
                    role="checkbox"
                    aria-checked={busy}
                    aria-label={`Mark “${t.title}” done`}
                    disabled={busy || editing}
                    onClick={() => complete(t)}
                    data-home-todo-complete={t.id}
                  >
                    {busy && <Check strokeWidth={3} />}
                  </button>
                  <button type="button" className="tk-main" onClick={() => setOpenId(t.id)} data-home-todo-open={t.id}>
                    <b>{t.title}</b>
                    <small>{t.detail || (t.source === "event" ? "Automatic" : t.assignedBy ? "Assigned to you" : "Personal")}</small>
                  </button>
                  {badge && <Badge kind={badge.kind}>{badge.label}</Badge>}
                </div>
              );
            })}
            {more > 0 && (
              <button type="button" className="tk tk-more" onClick={() => navigate("/todos")} data-todos-more>
                <small>+{more} more open</small>
              </button>
            )}
          </>
        )}
      </div>
      <SidePanel
        open={!!current}
        onClose={() => setOpenId(null)}
        title={current?.title ?? ""}
        eyebrow={`To-do · ${current?.source === "event" ? "Automatic" : current?.assignedBy ? "Assigned to you" : "Personal"}`}
        chip={current ? (dueBadge(current) ?? { label: "Open", kind: "neutral" }) : undefined}
        facts={
          current
            ? [
                { label: "Detail", value: current.detail || "—" },
                { label: "Due", value: current.dueAt ? new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short", year: "numeric", hour: "numeric", minute: "2-digit" }).format(new Date(current.dueAt)) : "No due date" },
                { label: "Added", value: relativeTime(current.createdAt) },
              ]
            : undefined
        }
        actions={
          current && (
            <>
              <Button icon={<Check strokeWidth={1.75} />} iconPosition="start" disabled={!!completing[current.id]} onClick={() => complete(current)} data-panel-complete>
                Mark done
              </Button>
              {current.link && (
                <Button variant="ghost" icon={<ArrowUpRight strokeWidth={1.75} />} onClick={() => navigate(current.link!)}>
                  Open
                </Button>
              )}
            </>
          )
        }
      />
    </BentoTile>
  );
}

/** Mock mode: the persona's fixture to-dos (presentation only, never sent anywhere). */
function MockTasks({ tile }: WidgetProps) {
  const { role } = useSession();
  const cfg = ROLE_CONFIG[role];
  const navigate = useNavigate();
  const toast = useToast();
  const [done, setDone] = useState<Record<number, boolean>>({});
  const [openTodo, setOpenTodo] = useState<number | null>(null);
  const todo: Todo | undefined = openTodo === null ? undefined : cfg.todos[openTodo];
  const parts = todo?.meta.split(" · ") ?? [];
  const finish = (verb: "Approved" | "Marked done") => {
    if (openTodo === null || !todo) return;
    setDone((d) => ({ ...d, [openTodo]: true }));
    toast(`${verb}: ${todo.title}`);
  };
  const closed = Object.values(done).filter(Boolean).length;
  const isDone = openTodo !== null && !!done[openTodo];
  return (
    <BentoTile className="dh-tasks">
      <TodoHead title={tile.title ?? "To-dos"} done={closed} total={cfg.todos.length} onAll={() => navigate("/todos")} />
      {cfg.todos.map((t, i) => (
        <div key={t.title} className={`tk${done[i] ? " done" : ""}`}>
          <button type="button" className="box" role="checkbox" aria-checked={!!done[i]} aria-label={`Mark “${t.title}” done`} onClick={() => setDone((d) => ({ ...d, [i]: !d[i] }))}>
            {done[i] && <Check strokeWidth={3} />}
          </button>
          <button type="button" className="tk-main" onClick={() => setOpenTodo(i)}>
            <b>{t.title}</b>
            <small>{t.meta}</small>
          </button>
          {done[i] ? <Badge kind="done">Done</Badge> : <Badge kind={t.status}>{BADGE_LABEL[t.status]}</Badge>}
        </div>
      ))}
      <SidePanel
        open={todo !== undefined}
        onClose={() => setOpenTodo(null)}
        title={todo?.title ?? ""}
        eyebrow={`To-do · ${parts[0] ?? ""}`}
        chip={todo ? (isDone ? { label: "Done", kind: "done" } : { label: BADGE_LABEL[todo.status], kind: todo.status }) : undefined}
        facts={
          todo
            ? [
                { label: "Area", value: parts[0] },
                { label: "From", value: parts.slice(1, -1).join(" · ") || parts[0] },
                { label: "Due", value: parts[parts.length - 1] },
              ]
            : undefined
        }
        actions={
          <>
            <Button icon={<Check strokeWidth={1.75} />} iconPosition="start" disabled={isDone} onClick={() => finish("Marked done")}>
              Mark done
            </Button>
            <Button variant="ghost" icon={<BadgeCheck strokeWidth={1.75} />} iconPosition="start" disabled={isDone} onClick={() => finish("Approved")}>
              Approve
            </Button>
          </>
        }
      >
        <p style={{ fontSize: 14, color: "var(--sb-body)", lineHeight: 1.55 }}>Opened from your home queue. Review the details, then mark it done or approve it to clear it from today’s list.</p>
      </SidePanel>
    </BentoTile>
  );
}

export function TasksWidget(p: WidgetProps) {
  return USES_API ? <LiveTasks {...p} /> : <MockTasks {...p} />;
}

/* ------------------------------------------------------------------ activity, brief, week */

export function NotifsWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const inbox = useInbox();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: [...inboxKeys.notifications(role, false), "home"], queryFn: () => inbox.notifications({ limit: 6 }) });
  const items = q.data?.items ?? [];
  return (
    <BentoTile title={tile.title ?? "Recent activity"} action={<button type="button" onClick={() => navigate("/notifications")}>See all</button>}>
      {q.isLoading ? (
        <div className="sb-skeleton" style={{ width: "70%" }} />
      ) : items.length === 0 ? (
        <EmptyState icon={BellOff} title="You’re all caught up" compact />
      ) : (
        <div className="dnts">
          {items.map((n) => (
            <button key={n.id} type="button" className={`sb-pop-item${n.readAt ? " is-read" : ""}`} onClick={() => n.link && navigate(n.link)}>
              <IconChip icon={Bell} tone="violet" size="sm" />
              <div>
                <b>{n.title}</b>
                <small>{relativeTime(n.createdAt)}</small>
              </div>
              <i className="sb-unread" />
            </button>
          ))}
        </div>
      )}
    </BentoTile>
  );
}

export function BriefWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const inbox = useInbox();
  const navigate = useNavigate();
  const unread = useQuery({ queryKey: inboxKeys.unread(role), queryFn: () => inbox.unreadCount() });
  const todos = useQuery({ queryKey: inboxKeys.todos(role, "open"), queryFn: () => inbox.todos("open") });
  const notices = useQuery({ queryKey: inboxKeys.notices(role, "current"), queryFn: () => inbox.notices("current") });
  const since = Date.now() - 86_400_000;
  const fresh = (notices.data?.items ?? []).filter((n) => new Date(n.publishAt).getTime() >= since).length;
  const overdue = (todos.data?.items ?? []).filter((t) => t.status === "open" && t.overdue).length;
  const lines: [string, string][] = [
    [`${formatNumber(unread.data ?? 0)} unread notification${unread.data === 1 ? "" : "s"}`, "/notifications"],
    [`${formatNumber(todos.data?.openCount ?? 0)} open to-do${todos.data?.openCount === 1 ? "" : "s"}${overdue ? ` · ${overdue} overdue` : ""}`, "/todos"],
    [`${formatNumber(fresh)} new notice${fresh === 1 ? "" : "s"} since yesterday`, "/notices"],
  ];
  return (
    <BentoTile className="dh-brief" title={tile.title ?? "Morning brief"}>
      <p className="dh-sub">
        <Sunrise strokeWidth={1.75} /> Your first look at the day
      </p>
      {lines.map(([l, to]) => (
        <button key={to} type="button" className="sb-row" onClick={() => navigate(to)}>
          <div>
            <b>{l}</b>
          </div>
          <ChevronRight strokeWidth={1.75} />
        </button>
      ))}
    </BentoTile>
  );
}

export function WeekWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const inbox = useInbox();
  const all = useQuery({ queryKey: inboxKeys.todos(role, "all"), queryFn: () => inbox.todos("all") });
  const notices = useQuery({ queryKey: inboxKeys.notices(role, "current"), queryFn: () => inbox.notices("current") });
  const now = new Date();
  const from = startOfWeek(now);
  const items = all.data?.items ?? [];
  const completed = items.filter((t) => t.completedAt && new Date(t.completedAt).getTime() >= from).length;
  const dismissed = items.filter((t) => t.dismissedAt && new Date(t.dismissedAt).getTime() >= from).length;
  const added = items.filter((t) => new Date(t.createdAt).getTime() >= from).length;
  const posted = (notices.data?.items ?? []).filter((n) => new Date(n.publishAt).getTime() >= from).length;
  const total = completed + dismissed;
  const cells: [number, string][] = [
    [completed, "To-dos completed"],
    [dismissed, "To-dos dismissed"],
    [added, "New to-dos"],
    [posted, "Notices this week"],
  ];
  return (
    <BentoTile className="dh-week">
      <h4>
        {tile.title ?? "Your week"}
        <span className="key">
          {formatWeekdayDate(new Date(from).toISOString())} – {formatWeekdayDate(now.toISOString())}
        </span>
      </h4>
      <p className="dh-big" data-week-total={total}>
        <b>{all.isLoading ? "…" : formatNumber(total)}</b> things closed so far
      </p>
      <div className="dh-cells">
        {cells.map(([n, l]) => (
          <div key={l}>
            <b>{all.isLoading ? "…" : formatNumber(n)}</b>
            <small>{l}</small>
          </div>
        ))}
      </div>
    </BentoTile>
  );
}

/* ------------------------------------------------------------------ admin early flags */

function FlagRow({ icon, tone, title, sub, cta, onClick }: { icon: LucideIcon; tone: Tone; title: string; sub: string; cta: string; onClick: () => void }) {
  return (
    <div className="dflag">
      <IconChip icon={icon} tone={tone} size="sm" />
      <div>
        <b>{title}</b>
        <small>{sub}</small>
      </div>
      <button type="button" className="sb-btn sb-btn--ghost sb-btn--sm" onClick={onClick}>
        {cta}
      </button>
    </div>
  );
}

export function FlagsWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const navigate = useNavigate();
  const inbox = useInbox();
  const { current: institution } = useInstitutions();
  const analytics = useAnalyticsApi();
  const metrics = useQuery({ queryKey: akeys(institution?.id ?? "-", "metrics"), queryFn: () => analytics.metrics(institution!.id), enabled: !!institution });
  const todos = useQuery({ queryKey: inboxKeys.todos(role, "open"), queryFn: () => inbox.todos("open") });
  const overdue = (todos.data?.items ?? []).filter((t) => t.status === "open" && t.overdue).length;
  const sla = Number(metricValue(metrics.data, "campus.tickets_past_sla") ?? 0);
  const owed = metricValue(metrics.data, "finance.receivables_outstanding");
  const flags: ReactNode[] = [];
  if (overdue > 0) flags.push(<FlagRow key="todo" icon={ListTodo} tone="rose" title={`${overdue} to-do${overdue === 1 ? " is" : "s are"} overdue`} sub="Past their due date in your queue" cta="Review" onClick={() => navigate("/todos")} />);
  if (sla > 0) flags.push(<FlagRow key="sla" icon={Wrench} tone="amber" title={`${formatNumber(sla)} service ticket${sla === 1 ? " is" : "s are"} past SLA`} sub="Campus maintenance response time" cta="Open" onClick={() => navigate(routeFor("campus/facilities"))} />);
  if (owed && /[1-9]/.test(owed)) flags.push(<FlagRow key="fees" icon={Wallet} tone="violet" title={`${formatInr(owed).replace(/\.00$/, "")} in fees outstanding`} sub="Receivables not yet collected" cta="Overdue list" onClick={() => navigate(routeFor("finance/receivables"))} />);
  return (
    <BentoTile className="dflags">
      <h4>
        {tile.title ?? "Early flags"}
        <span className="key">{flags.length ? `${flags.length} to watch` : "All normal"}</span>
      </h4>
      {flags.length ? flags : <EmptyState icon={TriangleAlert} title="Nothing is drifting from normal" compact />}
    </BentoTile>
  );
}

/* ------------------------------------------------------------------ student attendance buffer */

/** Classes that can still be missed while staying at or above the threshold. */
export function canMiss(attended: number, total: number, threshold: number): number {
  if (threshold <= 0) return Infinity;
  return Math.max(0, Math.floor((attended * 100) / threshold - total));
}

export function BunkWidget({ tile }: WidgetProps) {
  const api = useDeliveryApi();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: dkeys.scoped("me", "attendance"), queryFn: () => api.myAttendance() });
  const lines = (q.data?.items ?? []).filter((l) => l.total > 0);
  return (
    <BentoTile className="dh-bunk" title={tile.title ?? "Attendance buffer"} action={<button type="button" onClick={() => navigate(routeFor("timetable"))}>Timetable</button>}>
      {q.isLoading ? (
        <div className="sb-skeleton" style={{ width: "70%" }} />
      ) : lines.length === 0 ? (
        <EmptyState icon={CircleCheck} title="No sessions recorded yet" compact />
      ) : (
        lines.map((l) => {
          const left = canMiss(l.attended, l.total, l.thresholdPercent);
          const need = l.belowThreshold ? Math.max(1, Math.ceil((l.thresholdPercent * l.total - 100 * l.attended) / (100 - l.thresholdPercent))) : 0;
          return (
            <div key={l.sectionId} className="sb-row">
              <div>
                <b>{l.title}</b>
                <small>
                  {l.offeringCode} · {formatNumber(l.attended)} of {formatNumber(l.total)} · {Math.round(l.percent)}%
                </small>
              </div>
              {l.belowThreshold ? <Badge kind="attention">{`need ${need}`}</Badge> : <Badge kind={left > 2 ? "done" : "review"}>{`${left} left`}</Badge>}
            </div>
          );
        })
      )}
    </BentoTile>
  );
}

/* ------------------------------------------------------------------ pinned workspaces, sticky note */

const RECENT_KEY = "sutra.ws.recent";
/** Remembers a workspace the user opened (pinned-workspaces tile). */
export function rememberWorkspace(key: string) {
  try {
    const cur = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]") as string[];
    localStorage.setItem(RECENT_KEY, JSON.stringify([key, ...cur.filter((k) => k !== key)].slice(0, 8)));
  } catch {
    /* private mode */
  }
}
function recentWorkspaces(): string[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]") as string[];
  } catch {
    return [];
  }
}

export function PinnedWidget({ tile }: WidgetProps) {
  const { role } = useSession();
  const navigate = useNavigate();
  const ws = useWorkspaces(role as Role);
  const all = ws.data?.items ?? [];
  const recent = recentWorkspaces();
  const picked = [...recent.map((k) => all.find((w) => w.key === k)).filter((w): w is NonNullable<typeof w> => !!w), ...all.filter((w) => !recent.includes(w.key))].slice(0, 4);
  return (
    <BentoTile title={tile.title ?? "Pinned workspaces"} action={<button type="button" onClick={() => navigate("/workspaces")}>All</button>}>
      <div className="dh-pins">
        {picked.map((w) => {
          const g = groupMeta(w.group);
          return (
            <button key={w.key} type="button" className="sb-row" onClick={() => navigate(`/workspaces/${w.key}`)}>
              <IconChip icon={g.icon} tone={g.tone} size="sm" />
              <div>
                <b>{w.name}</b>
                <small>{w.key}</small>
              </div>
            </button>
          );
        })}
      </div>
    </BentoTile>
  );
}

export function NoteWidget({ tile, editing, onTileText }: WidgetProps) {
  const [text, setText] = useState(tile.text ?? "");
  return (
    <BentoTile className="dh-note" title={tile.title ?? "Sticky note"}>
      <textarea
        className="dh-note-text"
        aria-label="Sticky note"
        placeholder="Write a private note…"
        value={text}
        readOnly={editing}
        onChange={(e) => setText(e.target.value)}
        onBlur={() => onTileText?.(tile.id, text)}
        data-note={tile.id}
      />
    </BentoTile>
  );
}

export const WIDGET_COMPONENTS: Record<HomeTile["type"], (p: WidgetProps) => ReactNode> = {
  hero: HeroWidget,
  now: NowWidget,
  kpi: KpiWidget,
  tasks: TasksWidget,
  shortcuts: ShortcutsWidget,
  notice: NoticeWidget,
  notifs: NotifsWidget,
  pinned: PinnedWidget,
  brief: BriefWidget,
  note: NoteWidget,
  flags: FlagsWidget,
  week: WeekWidget,
  bunk: BunkWidget,
};

/** Library icons per widget type. */
export const WIDGET_ICONS: Record<HomeTile["type"], [LucideIcon, Tone]> = {
  hero: [Sun, "orange"],
  now: [Bell, "grey"],
  kpi: [CircleCheck, "blue"],
  tasks: [ListTodo, "orange"],
  shortcuts: [ArrowUpRight, "amber"],
  notice: [Megaphone, "green"],
  notifs: [Bell, "violet"],
  pinned: [ChevronRight, "rose"],
  brief: [Sunrise, "orange"],
  note: [ListTodo, "amber"],
  flags: [TriangleAlert, "rose"],
  week: [CircleCheck, "teal"],
  bunk: [CircleCheck, "blue"],
};
