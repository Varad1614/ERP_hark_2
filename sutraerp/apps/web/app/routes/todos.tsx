import { ArrowUpRight, Check, ListTodo, Plus, RotateCcw, X } from "lucide-react";
import { ApiProblem, type Todo } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, EmptyState, FilterChip, IconChip, Input, Modal, useToast } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { useSession } from "../lib/auth";
import { inboxKeys, useInbox, useInboxRefresh, type TodoAction, type TodoFilter } from "../lib/inbox";

export function meta() {
  return [{ title: "To-dos · SutraERP" }];
}

const FILTERS: { id: TodoFilter; label: string }[] = [
  { id: "open", label: "Open" },
  { id: "completed", label: "Done" },
  { id: "dismissed", label: "Dismissed" },
];

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
            placeholder="aarav.desai@sutra.dev"
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

/** Personal to-do inbox: open / done / dismissed, with complete, dismiss and reopen. */
export default function Todos() {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const navigate = useNavigate();
  const [filter, setFilter] = useState<TodoFilter>("open");
  const [composing, setComposing] = useState(false);
  const q = useQuery({ queryKey: inboxKeys.todos(role, filter), queryFn: () => inbox.todos(filter) });
  const items = q.data?.items ?? [];

  const act = async (t: Todo, action: TodoAction) => {
    try {
      await inbox.setTodo(t.id, action);
      await refresh();
      toast(action === "complete" ? `Done: ${t.title}` : action === "dismiss" ? `Dismissed: ${t.title}` : `Reopened: ${t.title}`);
    } catch (e) {
      toast(e instanceof Error ? e.message : "Could not update");
    }
  };

  return (
    <BentoGrid data-screen="todos">
      <BentoTile
        span={12}
        title="To-dos"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setComposing(true)} data-new-todo>
            New to-do
          </Button>
        }
      >
        <div className="sb-actions-row">
          {FILTERS.map((f) => (
            <FilterChip key={f.id} active={filter === f.id} label={f.label} count={f.id === "open" ? q.data?.openCount : undefined} onClick={() => setFilter(f.id)} />
          ))}
        </div>
        {q.isLoading ? (
          <div className="sb-skeleton" style={{ width: "50%" }} />
        ) : q.isError ? (
          <EmptyState icon={ListTodo} title="To-dos are unavailable">
            {q.error instanceof Error ? q.error.message : "Try again shortly."}
          </EmptyState>
        ) : items.length === 0 ? (
          <EmptyState icon={ListTodo} title={filter === "open" ? "Nothing to do — enjoy the calm" : "Nothing here"} />
        ) : (
          <div className="sb-list" data-todo-list>
            {items.map((t) => (
              <article key={t.id} className={`sb-list-row${t.status === "open" ? "" : " is-muted"}`} data-todo={t.id}>
                <IconChip icon={ListTodo} tone={t.overdue ? "rose" : "violet"} size="sm" />
                <div>
                  <h3>
                    {t.title}
                    {t.overdue && <Badge kind="attention">Overdue</Badge>}
                    {t.source === "event" && <Badge kind="info">Automatic</Badge>}
                    {t.assignedBy && <Badge kind="review">Assigned</Badge>}
                  </h3>
                  {t.detail && <p>{t.detail}</p>}
                  {t.dueAt && <small>Due {due(t.dueAt)}</small>}
                </div>
                <div className="sb-row-actions">
                  {t.link && (
                    <Button size="sm" variant="ghost" icon={<ArrowUpRight strokeWidth={1.75} />} onClick={() => navigate(t.link ?? "/")}>
                      Open
                    </Button>
                  )}
                  {t.status === "open" ? (
                    <>
                      <Button size="sm" icon={<Check strokeWidth={1.75} />} onClick={() => act(t, "complete")} data-complete={t.id}>
                        Done
                      </Button>
                      <Button size="sm" variant="ghost" icon={<X strokeWidth={1.75} />} onClick={() => act(t, "dismiss")} data-dismiss={t.id}>
                        Dismiss
                      </Button>
                    </>
                  ) : (
                    <Button size="sm" variant="ghost" icon={<RotateCcw strokeWidth={1.75} />} onClick={() => act(t, "reopen")} data-reopen={t.id}>
                      Reopen
                    </Button>
                  )}
                </div>
              </article>
            ))}
          </div>
        )}
      </BentoTile>
      <Modal open={composing} onClose={() => setComposing(false)} label="New to-do">
        <TodoForm canAssign={role !== "student"} onDone={() => setComposing(false)} />
      </Modal>
    </BentoGrid>
  );
}
