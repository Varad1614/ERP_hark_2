import { ArrowLeft, ArrowUpRight, Check, CircleCheck, ListTodo, Plus, RotateCcw, X } from "lucide-react";
import { ApiProblem, type Todo } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, EmptyState, Input, Modal, SidePanel, useToast } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { useSession } from "../lib/auth";
import { inboxKeys, useInbox, useInboxRefresh, type TodoAction } from "../lib/inbox";
import "../features/home/home.css";
import { dueBadge } from "../features/home/widgets";

export function meta() {
  return [{ title: "To-dos · SutraERP" }];
}


const due = (iso: string) => new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short", hour: "numeric", minute: "2-digit" }).format(new Date(iso));

function TodoForm({ canAssign, onDone }: { canAssign: boolean; onDone: () => void }) {
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const [title, setTitle] = useState("");
  const [detail, setDetail] = useState("");
  const [dueAt, setDueAt] = useState("");
  const [assignee, setAssignee] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!title.trim()) {
      setErrors({ title: "What needs doing?" });
      return;
    }
    setBusy(true);
    setErrors({});
    try {
      await inbox.createTodo({
        title,
        detail,
        dueAt: dueAt ? new Date(dueAt).toISOString() : undefined,
        assigneeLoginId: canAssign && assignee.trim() ? assignee.trim() : undefined,
      });
      await refresh();
      toast(assignee.trim() ? `Assigned to ${assignee.trim()}` : "To-do added");
      onDone();
    } catch (err) {
      if (err instanceof ApiProblem && err.problem.validationErrors?.length) {
        setErrors(Object.fromEntries(err.problem.validationErrors.map((x) => [x.field, x.message])));
      } else toast(err instanceof Error ? err.message : "Could not save");
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} noValidate data-todo-form>
      <h2>New to-do</h2>
      <div className="sb-form">
        <div className="sb-span-all">
          <Input label="Title" name="title" value={title} maxLength={200} error={errors.title} onChange={(e) => setTitle(e.target.value)} />
        </div>
        <label className="sb-field sb-span-all">
          Details (optional)
          <textarea className="sb-textarea" name="detail" value={detail} maxLength={4000} onChange={(e) => setDetail(e.target.value)} />
        </label>
        <Input label="Due (optional)" type="datetime-local" name="dueAt" value={dueAt} onChange={(e) => setDueAt(e.target.value)} />
        {canAssign && (
          <Input
            label="Assign to (login, optional)"
            name="assignee"
            placeholder="name@college.edu"
            value={assignee}
            error={errors.assigneeLoginId}
            onChange={(e) => setAssignee(e.target.value)}
          />
        )}
      </div>
      <div className="sb-actions-row">
        <Button type="submit" disabled={busy}>{busy ? "Saving…" : "Add to-do"}</Button>
        <Button variant="ghost" onClick={onDone} disabled={busy}>Cancel</Button>
      </div>
    </form>
  );
}

const fullDue = (iso: string) => new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short", year: "numeric", hour: "numeric", minute: "2-digit" }).format(new Date(iso));
const origin = (t: Todo) => (t.source === "event" ? "Automatic" : t.assignedBy ? "Assigned to you" : "Personal");

/** Badge of a to-do row: the due badge of the Home tile (overdue / due today / tomorrow / date), done or dismissed. */
function rowBadge(t: Todo): { label: string; kind: "attention" | "review" | "neutral" | "done" } | null {
  if (t.status === "completed") return { label: "Done", kind: "done" };
  if (t.status === "dismissed") return { label: "Dismissed", kind: "neutral" };
  return dueBadge(t);
}
/** Due date first (soonest on top), undated last. */
const byDue = (a: Todo, b: Todo) => (a.dueAt ?? "9999").localeCompare(b.dueAt ?? "9999");

function TodoRow({ t, onToggle, onOpen }: { t: Todo; onToggle: () => void; onOpen: () => void }) {
  const done = t.status === "completed";
  const badge = rowBadge(t);
  return (
    <div className={`tk${done ? " done" : ""}${t.status === "dismissed" ? " is-muted" : ""}`} data-todo={t.id}>
      {t.status !== "dismissed" && (
        <button type="button" className="box" role="checkbox" aria-checked={done} aria-label={done ? `Reopen “${t.title}”` : `Mark “${t.title}” done`} onClick={onToggle} data-complete={done ? undefined : t.id} data-reopen={done ? t.id : undefined}>
          {done && <Check strokeWidth={3} />}
        </button>
      )}
      <button type="button" className="tk-main" onClick={onOpen} data-row-open="">
        <b>{t.title}</b>
        <small>{[t.detail, t.dueAt ? due(t.dueAt) : null, origin(t)].filter(Boolean).join(" · ")}</small>
      </button>
      {badge && <Badge kind={badge.kind}>{badge.label}</Badge>}
    </div>
  );
}

/** Personal to-do inbox (prototype `/todos`): Open and Done tiles, dismissed behind a toggle, detail in a side panel. */
export default function Todos() {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const navigate = useNavigate();
  const [composing, setComposing] = useState(false);
  const [showDismissed, setShowDismissed] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const open = useQuery({ queryKey: inboxKeys.todos(role, "open"), queryFn: () => inbox.todos("open") });
  const done = useQuery({ queryKey: inboxKeys.todos(role, "completed"), queryFn: () => inbox.todos("completed") });
  const dismissed = useQuery({ queryKey: inboxKeys.todos(role, "dismissed"), queryFn: () => inbox.todos("dismissed") });
  const openItems = (open.data?.items ?? []).filter((t) => t.status === "open").sort(byDue);
  const doneItems = (done.data?.items ?? []).filter((t) => t.status === "completed");
  const dismissedItems = (dismissed.data?.items ?? []).filter((t) => t.status === "dismissed");
  const all = [...openItems, ...doneItems, ...dismissedItems];
  const current = all.find((t) => t.id === openId);
  const roleTitle = role === "administrator" ? "Administrator" : role === "faculty" ? "Faculty" : "Student";

  const act = async (t: Todo, action: TodoAction) => {
    try {
      await inbox.setTodo(t.id, action);
      await refresh();
      setOpenId(null);
      toast(action === "complete" ? `Done: ${t.title}` : action === "dismiss" ? `Dismissed: ${t.title}` : `Reopened: ${t.title}`);
    } catch (e) {
      toast(e instanceof Error ? e.message : "Could not update");
    }
  };
  const right = showDismissed ? dismissedItems : doneItems;

  return (
    <BentoGrid data-screen="todos">
      <BentoTile span={12} className="sb-area-head" data-area-head="">
        <div className="sb-area-title">
          <h1>Your to-dos</h1>
          <p className="sb-area-summary" data-area-summary="">
            {open.isLoading ? "Loading…" : `${openItems.length} open · ${doneItems.length} done · ${roleTitle}`}
          </p>
        </div>
        <div className="sb-area-side">
          <Button variant="ghost" icon={<Plus strokeWidth={1.75} />} iconPosition="start" onClick={() => setComposing(true)} data-new-todo>
            New to-do
          </Button>
          <Button variant="ghost" icon={<ArrowLeft strokeWidth={1.75} />} iconPosition="start" onClick={() => navigate("/")} data-back-home>
            Back to home
          </Button>
        </div>
      </BentoTile>
      <BentoTile span={8} title="Open" action={<span className="key">{openItems.length}</span>} data-todo-section="open">
        {open.isLoading ? (
          <div className="sb-skeleton" style={{ width: "50%" }} />
        ) : open.isError ? (
          <EmptyState icon={ListTodo} title="To-dos are unavailable">
            {open.error instanceof Error ? open.error.message : "Try again shortly."}
          </EmptyState>
        ) : openItems.length === 0 ? (
          <EmptyState icon={CircleCheck} title="All clear for today" compact>
            Everything in your queue is done.
          </EmptyState>
        ) : (
          <div data-todo-list>
            {openItems.map((t) => (
              <TodoRow key={t.id} t={t} onToggle={() => act(t, "complete")} onOpen={() => setOpenId(t.id)} />
            ))}
          </div>
        )}
      </BentoTile>
      <BentoTile
        span={4}
        title={showDismissed ? "Dismissed" : "Done"}
        action={
          <>
            <span className="key">{right.length}</span>
            <button type="button" onClick={() => setShowDismissed((v) => !v)} data-show-dismissed>
              {showDismissed ? "Show done" : `Dismissed${dismissedItems.length ? ` · ${dismissedItems.length}` : ""}`}
            </button>
          </>
        }
        data-todo-section={showDismissed ? "dismissed" : "done"}
      >
        {right.length === 0 ? (
          <EmptyState icon={ListTodo} title={showDismissed ? "Nothing dismissed" : "Nothing done yet"} compact />
        ) : (
          right.map((t) => <TodoRow key={t.id} t={t} onToggle={() => act(t, "reopen")} onOpen={() => setOpenId(t.id)} />)
        )}
      </BentoTile>
      <SidePanel
        open={!!current}
        onClose={() => setOpenId(null)}
        title={current?.title ?? ""}
        eyebrow={current ? `To-do · ${origin(current)}` : undefined}
        chip={current ? (rowBadge(current) ?? { label: "Open", kind: "neutral" }) : undefined}
        facts={
          current
            ? [
                { label: "Detail", value: current.detail || "—" },
                { label: "Due", value: current.dueAt ? fullDue(current.dueAt) : "No due date" },
                { label: "Status", value: current.status === "open" ? "Open" : current.status === "completed" ? "Done" : "Dismissed" },
              ]
            : undefined
        }
        actions={
          current && (
            <>
              {current.status === "open" ? (
                <Button icon={<Check strokeWidth={1.75} />} iconPosition="start" onClick={() => act(current, "complete")} data-panel-complete>
                  Mark done
                </Button>
              ) : (
                <Button icon={<RotateCcw strokeWidth={1.75} />} iconPosition="start" onClick={() => act(current, "reopen")} data-panel-reopen>
                  Reopen
                </Button>
              )}
              {current.link && (
                <Button variant="ghost" icon={<ArrowUpRight strokeWidth={1.75} />} onClick={() => navigate(current.link ?? "/")}>
                  Open
                </Button>
              )}
              {current.status === "open" && (
                <Button variant="ghost" icon={<X strokeWidth={1.75} />} iconPosition="start" onClick={() => act(current, "dismiss")} data-dismiss={current.id}>
                  Dismiss
                </Button>
              )}
            </>
          )
        }
      />
      <Modal open={composing} onClose={() => setComposing(false)} label="New to-do">
        <TodoForm canAssign={role !== "student"} onDone={() => setComposing(false)} />
      </Modal>
    </BentoGrid>
  );
}
