import { Archive, Megaphone, Pin, Plus } from "lucide-react";
import { ApiProblem, type NewNotice, type Notice, type Role } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, EmptyState, FilterChip, IconChip, Input, Modal, useToast, type BadgeKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useSession } from "../lib/auth";
import { inboxKeys, useInbox, useInboxRefresh } from "../lib/inbox";
import { ROLE_CONFIG, ROLES } from "../lib/roles";

export function meta() {
  return [{ title: "Notices · SutraERP" }];
}

const STATE_BADGE: Record<Notice["state"], { kind: BadgeKind; label: string }> = {
  live: { kind: "done", label: "Live" },
  scheduled: { kind: "info", label: "Scheduled" },
  expired: { kind: "neutral", label: "Expired" },
  archived: { kind: "neutral", label: "Archived" },
};

const fmt = (iso: string) => new Intl.DateTimeFormat("en-IN", { dateStyle: "medium", timeStyle: "short" }).format(new Date(iso));

/** Client-side form validation mirroring the API's (the API stays authoritative). */
export function validateNotice(n: { title: string; body: string; audienceRoles: Role[]; publishAt?: string; expiresAt?: string }): Record<string, string> {
  const e: Record<string, string> = {};
  if (!n.title.trim()) e.title = "Give the notice a title";
  else if (n.title.trim().length > 200) e.title = "At most 200 characters";
  if (!n.body.trim()) e.body = "Write the announcement";
  if (n.audienceRoles.length === 0) e.audienceRoles = "Choose at least one audience";
  if (n.expiresAt && n.publishAt && n.expiresAt <= n.publishAt) e.expiresAt = "Must be after the publish time";
  return e;
}

const toIso = (local: string) => (local ? new Date(local).toISOString() : undefined);

function NoticeForm({ onDone }: { onDone: () => void }) {
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [audience, setAudience] = useState<Role[]>(["student", "faculty"]);
  const [pinned, setPinned] = useState(false);
  const [publishAt, setPublishAt] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const draft: NewNotice = { title, body, audienceRoles: audience, pinned, publishAt: toIso(publishAt), expiresAt: toIso(expiresAt) };
    const v = validateNotice({ ...draft, publishAt: draft.publishAt ?? undefined, expiresAt: draft.expiresAt ?? undefined });
    setErrors(v);
    if (Object.keys(v).length) return;
    setBusy(true);
    try {
      const n = await inbox.createNotice(draft);
      await refresh();
      toast(n.state === "scheduled" ? "Notice scheduled" : "Notice published");
      onDone();
    } catch (err) {
      if (err instanceof ApiProblem && err.problem.validationErrors) {
        setErrors(Object.fromEntries(err.problem.validationErrors.map((x) => [x.field, x.message])));
      } else toast(err instanceof Error ? err.message : "Could not publish");
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} noValidate data-notice-form>
      <h2>New notice</h2>
      <div className="sb-form">
        <div className="sb-span-all">
          <Input label="Title" name="title" value={title} maxLength={200} error={errors.title} onChange={(e) => setTitle(e.target.value)} />
        </div>
        <label className="sb-field sb-span-all">
          Announcement
          <textarea className="sb-textarea" name="body" value={body} maxLength={10000} onChange={(e) => setBody(e.target.value)} aria-invalid={errors.body ? true : undefined} />
          {errors.body && <span className="sb-field-error">{errors.body}</span>}
        </label>
        <fieldset className="sb-field sb-span-all">
          <legend>Audience</legend>
          <div className="sb-checks">
            {ROLES.map((r) => (
              <label key={r}>
                <input
                  type="checkbox"
                  name={`audience-${r}`}
                  checked={audience.includes(r)}
                  onChange={(e) => setAudience((a) => (e.target.checked ? [...a, r] : a.filter((x) => x !== r)))}
                />
                {ROLE_CONFIG[r].title}
              </label>
            ))}
            <label>
              <input type="checkbox" name="pinned" checked={pinned} onChange={(e) => setPinned(e.target.checked)} /> Pin to top
            </label>
          </div>
          {errors.audienceRoles && <span className="sb-field-error">{errors.audienceRoles}</span>}
        </fieldset>
        <Input label="Publish at (optional)" type="datetime-local" name="publishAt" value={publishAt} onChange={(e) => setPublishAt(e.target.value)} />
        <Input label="Expires at (optional)" type="datetime-local" name="expiresAt" value={expiresAt} error={errors.expiresAt} onChange={(e) => setExpiresAt(e.target.value)} />
      </div>
      <div className="sb-actions-row">
        <Button type="submit" disabled={busy}>{busy ? "Publishing…" : "Publish"}</Button>
        <Button variant="ghost" onClick={onDone} disabled={busy}>Cancel</Button>
      </div>
    </form>
  );
}

/** Institution notices: readers see live notices for their role; managers publish and archive. */
export default function Notices() {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const canManage = role === "administrator";
  const [view, setView] = useState<"current" | "manage">("current");
  const [composing, setComposing] = useState(false);
  const q = useQuery({ queryKey: inboxKeys.notices(role, view), queryFn: () => inbox.notices(view) });
  const items = q.data?.items ?? [];

  const archive = async (n: Notice) => {
    try {
      await inbox.archiveNotice(n.id);
      await refresh();
      toast(`Archived: ${n.title}`);
    } catch (e) {
      toast(e instanceof Error ? e.message : "Could not archive");
    }
  };

  return (
    <BentoGrid data-screen="notices">
      <BentoTile
        span={12}
        title="Notices"
        action={
          canManage ? (
            <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setComposing(true)} data-new-notice>
              New notice
            </Button>
          ) : undefined
        }
      >
        {canManage && (
          <div className="sb-actions-row">
            <FilterChip active={view === "current"} label="For me" onClick={() => setView("current")} />
            <FilterChip active={view === "manage"} label="All notices" onClick={() => setView("manage")} />
          </div>
        )}
        {q.isLoading ? (
          <div className="sb-skeleton" style={{ width: "50%" }} />
        ) : q.isError ? (
          <EmptyState icon={Megaphone} title="Notices are unavailable">
            {q.error instanceof Error ? q.error.message : "Try again shortly."}
          </EmptyState>
        ) : items.length === 0 ? (
          <EmptyState icon={Megaphone} title="No notices right now">
            Announcements for your role appear here.
          </EmptyState>
        ) : (
          <div className="sb-list" data-notice-list>
            {items.map((n) => (
              <article key={n.id} className={`sb-list-row${n.state === "live" ? "" : " is-muted"}`} data-notice={n.id}>
                <IconChip icon={n.pinned ? Pin : Megaphone} tone={n.pinned ? "orange" : "blue"} size="sm" />
                <div>
                  <h3>
                    {n.title}
                    {n.pinned && <Badge kind="attention">Pinned</Badge>}
                    {view === "manage" && <Badge kind={STATE_BADGE[n.state].kind}>{STATE_BADGE[n.state].label}</Badge>}
                  </h3>
                  <p>{n.body}</p>
                  <small>
                    {n.authorName} · {fmt(n.publishAt)}
                    {n.expiresAt ? ` · until ${fmt(n.expiresAt)}` : ""}
                    {view === "manage" ? ` · for ${n.audienceRoles.map((r) => ROLE_CONFIG[r].title).join(", ")}` : ""}
                  </small>
                </div>
                {canManage && n.state !== "archived" && (
                  <div className="sb-row-actions">
                    <Button size="sm" variant="ghost" icon={<Archive strokeWidth={1.75} />} onClick={() => archive(n)} data-archive={n.id}>
                      Archive
                    </Button>
                  </div>
                )}
              </article>
            ))}
          </div>
        )}
      </BentoTile>
      <Modal open={composing} onClose={() => setComposing(false)} label="New notice">
        <NoticeForm onDone={() => setComposing(false)} />
      </Modal>
    </BentoGrid>
  );
}
