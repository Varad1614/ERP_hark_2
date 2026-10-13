import { ArrowUpRight, BadgeCheck, Check, CircleCheck, ListTodo, Sun } from "lucide-react";
import { AdminKpis, FacultyKpis, StudentKpis, RightNowTile, AnnouncementTile } from "../features/home/live";
import type { Todo as LiveTodo } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, Drawer, EmptyState, IconChip, useToast } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { USES_API } from "../lib/api";
import { useSession } from "../lib/auth";
import { inboxKeys, useInbox, useInboxRefresh } from "../lib/inbox";
import { BADGE_LABEL, ROLE_CONFIG, routeFor, type Todo } from "../lib/roles";

export function meta() {
  return [{ title: "Home · SutraERP" }];
}

function today(): string {
  return new Intl.DateTimeFormat("en-IN", { weekday: "long", day: "numeric", month: "long" }).format(new Date());
}

/** How many open to-dos the home card shows (the rest are one click away on /todos). */
export const HOME_TODO_LIMIT = 4;

const shortDue = (d: Date) => new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short" }).format(d);
const timeOfDay = (d: Date) => new Intl.DateTimeFormat("en-IN", { hour: "numeric", minute: "2-digit" }).format(d);

/** Due badge of a live to-do: overdue, due today, due tomorrow, or the date; none without a due date. */
export function dueBadge(t: Pick<LiveTodo, "dueAt" | "overdue">, now: Date = new Date()): { kind: "attention" | "review" | "neutral"; label: string } | null {
  if (!t.dueAt) return null;
  const due = new Date(t.dueAt);
  if (t.overdue || due.getTime() < now.getTime()) return { kind: "attention", label: "Overdue" };
  const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOfDay(due) - startOfDay(now)) / 86_400_000);
  if (days === 0) return { kind: "attention", label: `Due ${timeOfDay(due)}` };
  if (days === 1) return { kind: "review", label: "Due tomorrow" };
  return { kind: "neutral", label: `Due ${shortDue(due)}` };
}

/**
 * "Your to-dos" with live data (API / OIDC mode): the first open items of
 * `GET /api/v1/todos?status=open`, due/overdue badges, the checkbox completes through
 * `POST /api/v1/todos/{id}/complete` (CSRF header from the client middleware).
 */
function LiveTodosTile() {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: inboxKeys.todos(role, "open"), queryFn: () => inbox.todos("open") });
  const [completing, setCompleting] = useState<Record<string, boolean>>({});
  const items = (q.data?.items ?? []).filter((t) => t.status === "open").slice(0, HOME_TODO_LIMIT);
  const more = Math.max(0, (q.data?.openCount ?? 0) - items.length);

  const complete = async (t: LiveTodo) => {
    if (completing[t.id]) return;
    setCompleting((c) => ({ ...c, [t.id]: true }));
    try {
      await inbox.setTodo(t.id, "complete");
      toast(`Done: ${t.title}`);
      await refresh();
    } catch (e) {
      toast(e instanceof Error ? `Couldn’t complete “${t.title}”: ${e.message}` : "Could not update");
    } finally {
      setCompleting((c) => Object.fromEntries(Object.entries(c).filter(([id]) => id !== t.id)));
    }
  };

  return (
    <BentoTile
      span={7}
      title={q.data && q.data.openCount > 0 ? `Your to-dos · ${q.data.openCount}` : "Your to-dos"}
      action={
        <button type="button" onClick={() => navigate("/todos")} data-todos-see-all>
          See all
        </button>
      }
    >
      <div data-home-todos={q.isLoading ? "loading" : q.isError ? "error" : items.length === 0 ? "empty" : "ready"}>
        {q.isLoading ? (
          <>
            <div className="sb-skeleton" style={{ width: "70%" }} />
            <div className="sb-skeleton" style={{ width: "55%" }} />
            <div className="sb-skeleton" style={{ width: "62%" }} />
          </>
        ) : q.isError ? (
          <EmptyState icon={ListTodo} title="To-dos are unavailable">
            <span role="alert">{q.error instanceof Error ? q.error.message : "Try again shortly."}</span>{" "}
            <Button size="sm" variant="ghost" onClick={() => q.refetch()} data-todos-retry>
              Retry
            </Button>
          </EmptyState>
        ) : items.length === 0 ? (
          <EmptyState icon={ListTodo} title="Nothing to do — enjoy the calm" />
        ) : (
          <>
            {items.map((t) => {
              const badge = dueBadge(t);
              const busy = !!completing[t.id];
              return (
                <div key={t.id} className={`sb-row${busy ? " is-done" : ""}`} data-home-todo={t.id}>
                  <button
                    type="button"
                    className="sb-check"
                    role="checkbox"
                    aria-checked={busy}
                    aria-label={`Mark “${t.title}” done`}
                    disabled={busy}
                    onClick={() => complete(t)}
                    data-home-todo-complete={t.id}
                  >
                    {busy && <Check strokeWidth={2} />}
                  </button>
                  <div role="link" tabIndex={0} style={{ cursor: "pointer" }} onClick={() => navigate(t.link ?? "/todos")} onKeyDown={(e) => e.key === "Enter" && navigate(t.link ?? "/todos")}>
                    <b>{t.title}</b>
                    <small>
                      {t.detail || (t.source === "event" ? "Automatic" : t.assignedBy ? "Assigned to you" : "Personal")}
                    </small>
                  </div>
                  {badge && <Badge kind={badge.kind}>{badge.label}</Badge>}
                </div>
              );
            })}
            {more > 0 && (
              <button type="button" className="sb-row" onClick={() => navigate("/todos")} data-todos-more>
                <small>+{more} more open</small>
              </button>
            )}
          </>
        )}
      </div>
    </BentoTile>
  );
}

export default function Home() {
  const { role } = useSession();
  const cfg = ROLE_CONFIG[role];
  const navigate = useNavigate();
  const toast = useToast();
  const [done, setDone] = useState<Record<number, boolean>>({});
  const [openTodo, setOpenTodo] = useState<number | null>(null);

  const go = (to: string) => {
    if (to.startsWith("todo:")) setOpenTodo(Number(to.slice(5)));
    else navigate(routeFor(to));
  };
  const todo: Todo | undefined = openTodo === null ? undefined : cfg.todos[openTodo];
  const parts = todo?.meta.split(" · ") ?? [];
  const finish = (verb: "Approved" | "Marked done") => {
    if (openTodo === null || !todo) return;
    setDone((d) => ({ ...d, [openTodo]: true }));
    toast(`${verb}: ${todo.title}`);
  };

  return (
    <BentoGrid data-home={role}>
      <BentoTile span={7} variant="accent" className="sb-hero">
        <span className="sb-chip">
          <Sun strokeWidth={1.75} /> {today()}
        </span>
        <h1>
          Hi {cfg.first},
          <br />
          let’s make today easy.
        </h1>
        <p>{cfg.hero.sub}</p>
        <Button className="sb-mt" icon={<ArrowUpRight strokeWidth={1.75} />} onClick={() => go(cfg.hero.to)}>
          {cfg.hero.cta}
        </Button>
      </BentoTile>

      <RightNowTile role={role} />

      {role === "administrator" ? <AdminKpis go={go} /> : role === "faculty" ? <FacultyKpis go={go} /> : <StudentKpis go={go} />}

      {USES_API ? (
        <LiveTodosTile />
      ) : (
        <BentoTile
          span={7}
          title="Your to-dos"
          action={
            <button type="button" onClick={() => navigate("/todos")}>
              See all
            </button>
          }
        >
          {cfg.todos.map((t, i) => (
            <button key={t.title} type="button" className={`sb-row${done[i] ? " is-done" : ""}`} onClick={() => setOpenTodo(i)}>
              <i className="sb-check">{done[i] && <Check strokeWidth={2} />}</i>
              <div>
                <b>{t.title}</b>
                <small>{t.meta}</small>
              </div>
              {done[i] ? <Badge kind="done">Done</Badge> : <Badge kind={t.status}>{BADGE_LABEL[t.status]}</Badge>}
            </button>
          ))}
        </BentoTile>
      )}

      <div className="sb-side sb-span-5">
        <BentoTile title="Shortcuts">
          <div className="sb-shortcuts">
            {cfg.shortcuts.map((s) => (
              <button key={s.label} type="button" className="sb-shortcut" onClick={() => go(s.to)}>
                <div className="sb-shortcut-icon">
                  <s.icon strokeWidth={1.75} />
                </div>
                {s.label}
              </button>
            ))}
          </div>
        </BentoTile>
        <AnnouncementTile />
      </div>

      <Drawer
        open={todo !== undefined}
        onClose={() => setOpenTodo(null)}
        title={todo?.title ?? ""}
        eyebrow={`To-do · ${parts[0] ?? ""}`}
        media={<IconChip icon={openTodo !== null && done[openTodo] ? CircleCheck : ListTodo} tone={openTodo !== null && done[openTodo] ? "green" : "orange"} />}
        footer={
          <>
            <Button icon={<Check strokeWidth={1.75} />} iconPosition="start" disabled={openTodo !== null && done[openTodo]} onClick={() => finish("Marked done")}>
              Mark done
            </Button>
            <Button variant="ghost" icon={<BadgeCheck strokeWidth={1.75} />} iconPosition="start" disabled={openTodo !== null && done[openTodo]} onClick={() => finish("Approved")}>
              Approve
            </Button>
          </>
        }
      >
        {todo && (
          <>
            <dl className="sb-dl">
              <dt>Area</dt>
              <dd>{parts[0]}</dd>
              <dt>From</dt>
              <dd>{parts.slice(1, -1).join(" · ") || parts[0]}</dd>
              <dt>Due</dt>
              <dd>{parts[parts.length - 1]}</dd>
              <dt>Status</dt>
              <dd>{openTodo !== null && done[openTodo] ? <Badge kind="done">Done</Badge> : <Badge kind={todo.status}>{BADGE_LABEL[todo.status]}</Badge>}</dd>
            </dl>
            <p style={{ fontSize: 14, color: "var(--sb-body)", lineHeight: 1.55 }}>
              Opened from your home queue. Review the details, then mark it done or approve it to clear it from today’s list.
            </p>
          </>
        )}
      </Drawer>
    </BentoGrid>
  );
}
