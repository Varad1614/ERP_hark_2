/**
 * ERP_EXTENSIONS 5A: the public careers page (`/careers/:slug`, no session). Lists the
 * institution's published openings and takes applications. `website` is the honeypot: hidden
 * from people (and from tab order / autofill), bots fill it, the server answers 202 and stores
 * nothing. 202 → "Application received"; 429 → the requisition's daily cap is reached.
 * `introHtml` / `descriptionHtml` arrive escaped by the server (`careers::description_html`).
 */
import { Briefcase } from "lucide-react";
import { ApiProblem } from "@sutra/api-client";
import { BentoGrid, BentoTile, Button, EmptyState, FormGrid, KeyValue, TextArea, TextField } from "@sutra/ui";
import { useState, type CSSProperties, type FormEvent } from "react";
import { defaultOffersApi, usePublicCareers, type OffersApi } from "./offersApi";
import type { PublicOpening } from "./offersTypes";

const HIDDEN: CSSProperties = { position: "absolute", left: "-10000px", width: 1, height: 1, overflow: "hidden" };
type Status = "idle" | "sending" | "received" | "limited" | "error";
const EMPTY = { name: "", email: "", phone: "", note: "", website: "" };

function ApplyForm({ slug, opening, api }: { slug: string; opening: PublicOpening; api: OffersApi }) {
  const [v, setV] = useState(EMPTY);
  const [status, setStatus] = useState<Status>("idle");
  const [error, setError] = useState("");
  const set = (k: keyof typeof EMPTY) => (e: { target: { value: string } }) => setV({ ...v, [k]: e.target.value });

  async function submit(e: FormEvent) {
    e.preventDefault();
    setStatus("sending");
    setError("");
    try {
      await api.apply(slug, opening.id, { name: v.name.trim(), email: v.email.trim(), phone: v.phone.trim() || null, note: v.note.trim() || null, website: v.website || null });
      setStatus("received");
      setV(EMPTY);
    } catch (err) {
      if (err instanceof ApiProblem && err.problem.status === 429) {
        setStatus("limited");
      } else {
        setStatus("error");
        setError(err instanceof Error ? err.message : "Could not send your application");
      }
    }
  }

  if (status === "received") return <p role="status" data-careers-received>Application received</p>;
  return (
    <form onSubmit={(e) => void submit(e)} data-careers-apply={opening.id}>
      <FormGrid columns={2}>
        <TextField label="Full name" name="name" required maxLength={200} autoComplete="name" value={v.name} onChange={set("name")} />
        <TextField label="Email" name="email" type="email" required maxLength={254} autoComplete="email" value={v.email} onChange={set("email")} />
        <TextField label="Phone" name="phone" type="tel" maxLength={40} autoComplete="tel" value={v.phone} onChange={set("phone")} />
        <TextArea label="Cover note" name="note" maxLength={4000} rows={4} value={v.note} onChange={set("note")} />
      </FormGrid>
      <div style={HIDDEN} aria-hidden="true">
        <label>
          Website
          <input type="text" name="website" tabIndex={-1} autoComplete="off" value={v.website} onChange={set("website")} />
        </label>
      </div>
      {status === "limited" && <p role="alert" data-careers-limited>Too many applications today. Please try again tomorrow.</p>}
      {status === "error" && <p role="alert">{error}</p>}
      <Button type="submit" size="sm" data-action="careers-apply" disabled={status === "sending" || !v.name.trim() || !v.email.trim()}>
        {status === "sending" ? "Sending…" : "Apply"}
      </Button>
    </form>
  );
}

export function CareersPage({ slug, api = defaultOffersApi() }: { slug: string; api?: OffersApi }) {
  const careers = usePublicCareers(slug, api);
  const [open, setOpen] = useState<string | null>(null);
  const d = careers.data;

  if (careers.isLoading) return <div data-careers-page><EmptyState icon={Briefcase} title="Loading openings" compact>One moment.</EmptyState></div>;
  if (!d) return <div data-careers-page><EmptyState icon={Briefcase} title="Careers page not found" compact>This link is not active.</EmptyState></div>;
  return (
    <div data-careers-page>
      <BentoGrid>
        <BentoTile span={12} title={`Careers at ${d.institution}`}>
          {/* Escaped by the server. */}
          <div dangerouslySetInnerHTML={{ __html: d.introHtml }} />
          {d.contactEmail && <KeyValue label="Questions" value={<a href={`mailto:${d.contactEmail}`}>{d.contactEmail}</a>} />}
        </BentoTile>
        {d.openings.length === 0 ? (
          <BentoTile span={12} title="Open roles">
            <EmptyState icon={Briefcase} title="No open roles right now" compact>Check back soon.</EmptyState>
          </BentoTile>
        ) : (
          d.openings.map((o) => (
            <BentoTile key={o.id} span={12} title={o.title} data-careers-opening={o.id}
              action={<Button size="sm" variant={open === o.id ? "ghost" : "primary"} data-action="careers-open-apply" onClick={() => setOpen(open === o.id ? null : o.id)}>{open === o.id ? "Close" : "Apply"}</Button>}>
              <KeyValue label="Role" value={[o.designation, o.staffCategory, o.employmentType].filter(Boolean).join(" · ")} />
              <KeyValue label="Openings" value={o.openings} />
              {o.location && <KeyValue label="Location" value={o.location} />}
              {o.closesOn && <KeyValue label="Apply by" value={o.closesOn} />}
              <div dangerouslySetInnerHTML={{ __html: o.descriptionHtml }} />
              {open === o.id && <ApplyForm slug={slug} opening={o} api={api} />}
            </BentoTile>
          ))
        )}
      </BentoGrid>
    </div>
  );
}
