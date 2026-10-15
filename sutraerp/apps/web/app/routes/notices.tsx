import { Archive, CalendarCheck, Check, Library, Megaphone, Plus, Wifi, type LucideIcon } from "lucide-react";
import { ApiProblem, type NewNotice, type Notice, type Role } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, EmptyState, IconChip, Input, Modal, SidePanel, useToast, type BadgeKind, type Tone } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useSession } from "../lib/auth";
import { inboxKeys, useInbox, useInboxRefresh } from "../lib/inbox";
import { ROLE_CONFIG, ROLES, roleTitle } from "../lib/roles";
import "../features/home/home.css";
import "../components/shell.css";

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

/** Icon + tone of a notice card, from its wording (prototype notice cards). */
function noticeLook(n: Notice): { icon: LucideIcon; tone: Tone } {
  const rules: [RegExp, LucideIcon, Tone][] = [
    [/exam|hall ticket|result|paper/, CalendarCheck, "violet"],
    [/library|book/, Library, "amber"],
    [/wi-?fi|network|maintenance|outage|it services/, Wifi, "blue"],
  ];
  // The title decides first; the body only when the title says nothing.
  for (const text of [n.title.toLowerCase(), n.body.toLowerCase()]) {
    const hit = rules.find(([re]) => re.test(text));
    if (hit) return { icon: hit[1], tone: hit[2] };
  }
  return { icon: Megaphone, tone: "green" };
}
const day = (iso: string) => new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "long" }).format(new Date(iso));

/** Institution notices (prototype `/notices`): readers see live notices for their role as cards; managers publish and archive. */
export default function Notices() {
  const { role } = useSession();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const canManage = role === "administrator";
  const [view, setView] = useState<"current" | "manage">("current");
  const [composing, setComposing] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const q = useQuery({ queryKey: inboxKeys.notices(role, view), queryFn: () => inbox.notices(view) });
  const items = [...(q.data?.items ?? [])].sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.publishAt.localeCompare(a.publishAt));
  const current = items.find((n) => n.id === openId);

  const archive = async (n: Notice) => {
    try {
      await inbox.archiveNotice(n.id);
      await refresh();
      setOpenId(null);
      toast(`Archived: ${n.title}`);
    } catch (e) {
      toast(e instanceof Error ? e.message : "Could not archive");
    }
  };

  return (
    <BentoGrid data-screen="notices">
      <BentoTile span={12} className="sb-area-head" data-area-head="">
        <div className="sb-area-title">
          <h1>Notices</h1>
          <p className="sb-area-summary" data-area-summary="">
            {q.isLoading ? "Loading…" : `${items.length} ${view === "manage" ? "notices in total" : `notice${items.length === 1 ? "" : "s"} for you`} · week of ${day(new Date().toISOString())}`}
          </p>
        </div>
        {canManage && (
          <div className="sb-area-side">
            <div className="dseg" role="radiogroup" aria-label="Notices to show">
              <button type="button" role="radio" aria-checked={view === "current"} className={view === "current" ? "on" : ""} onClick={() => setView("current")} data-notice-view="current">
                For me
              </button>
              <button type="button" role="radio" aria-checked={view === "manage"} className={view === "manage" ? "on" : ""} onClick={() => setView("manage")} data-notice-view="manage">
                All notices
              </button>
            </div>
            <Button icon={<Plus strokeWidth={1.75} />} onClick={() => setComposing(true)} data-new-notice>
              New notice
            </Button>
          </div>
        )}
      </BentoTile>
      {q.isLoading ? (
        <BentoTile span={12}>
          <div className="sb-skeleton" style={{ width: "50%" }} />
        </BentoTile>
      ) : q.isError ? (
        <BentoTile span={12}>
          <EmptyState icon={Megaphone} title="Notices are unavailable">
            {q.error instanceof Error ? q.error.message : "Try again shortly."}
          </EmptyState>
        </BentoTile>
      ) : items.length === 0 ? (
        <BentoTile span={12}>
          <EmptyState icon={Megaphone} title="No notices right now">
            Announcements for your role appear here.
          </EmptyState>
        </BentoTile>
      ) : (
        items.map((n) => {
          const look = noticeLook(n);
          return (
            <BentoTile key={n.id} span={6} as="button" type="button" interactive className={`nt-card${n.state === "live" ? "" : " is-muted"}`} onClick={() => setOpenId(n.id)} data-notice={n.id}>
              <IconChip icon={look.icon} tone={look.tone} />
              <div>
                <b>
                  {n.title}
                  {n.pinned && <Badge kind="attention">Pinned</Badge>}
                  {view === "manage" && <Badge kind={STATE_BADGE[n.state].kind}>{STATE_BADGE[n.state].label}</Badge>}
                </b>
                <p>{n.body}</p>
                <small>
                  {n.authorName} · {day(n.publishAt)}
                  {n.expiresAt ? ` · until ${day(n.expiresAt)}` : ""}
                  {view === "manage" ? ` · for ${n.audienceRoles.map((r) => roleTitle(r)).join(", ")}` : ""}
                </small>
              </div>
            </BentoTile>
          );
        })
      )}
      <SidePanel
        open={!!current}
        onClose={() => setOpenId(null)}
        title={current?.title ?? ""}
        eyebrow={current ? `Notice · ${current.authorName}` : undefined}
        facts={
          current
            ? [
                { label: "From", value: current.authorName },
                { label: "When", value: `${fmt(current.publishAt)}${current.expiresAt ? ` – ${fmt(current.expiresAt)}` : ""}` },
                { label: "For", value: current.audienceRoles.map((r) => roleTitle(r)).join(", ") },
                ...(canManage ? [{ label: "Status", value: STATE_BADGE[current.state].label }] : []),
              ]
            : undefined
        }
        actions={
          current && (
            <>
              <Button icon={<Check strokeWidth={1.75} />} iconPosition="start" onClick={() => setOpenId(null)} data-notice-ack>
                Got it
              </Button>
              {canManage && current.state !== "archived" && (
                <Button variant="ghost" icon={<Archive strokeWidth={1.75} />} iconPosition="start" onClick={() => archive(current)} data-archive={current.id}>
                  Archive
                </Button>
              )}
              <Button variant="ghost" onClick={() => setOpenId(null)}>
                Close
              </Button>
            </>
          )
        }
      >
        {current && <p className="nt-body">{current.body}</p>}
      </SidePanel>
      <Modal open={composing} onClose={() => setComposing(false)} label="New notice">
        <NoticeForm onDone={() => setComposing(false)} />
      </Modal>
    </BentoGrid>
  );
}
