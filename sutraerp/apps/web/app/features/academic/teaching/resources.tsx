/**
 * Re-skin v2 (R3): faculty Resources (prototype `fac-resources.png`, `-dlg-f2-up.png`): one tile
 * per taught section with `ListRow`s (type chip, kind · source · date, "Visible to students" /
 * Draft badge, one action + kebab) and an "Upload material" `StepDialog` (title, type, link, note,
 * Visible-to-students toggle). Same API calls (create with publish flag, publish / withdraw).
 */
import { BookOpen, ExternalLink, FileText, Library, Link2, Upload, Video } from "lucide-react";
import { AreaHeader, BentoTile, EmptyState, formatDateShort, ListRow, type Tone } from "@sutra/ui";
import type { LucideIcon } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { useCommand } from "../../institution/hooks";
import { fkeys, useFlexibleApi } from "../flexible/hooks";
import type { CourseResource, ResourceKind, TaughtSection } from "../flexible/types";
import { courseCode, useAsk } from "../kit";

const KINDS: { value: ResourceKind; label: string }[] = [
  { value: "reading", label: "Reading" },
  { value: "document", label: "Document" },
  { value: "link", label: "Link" },
  { value: "video", label: "Video" },
  { value: "library_item", label: "Library item" },
];
const KIND_LOOK: Record<string, { icon: LucideIcon; tone: Tone }> = {
  reading: { icon: BookOpen, tone: "amber" },
  document: { icon: FileText, tone: "rose" },
  link: { icon: Link2, tone: "teal" },
  video: { icon: Video, tone: "violet" },
  library_item: { icon: Library, tone: "blue" },
};
const kindLabel = (k: string) => KINDS.find((x) => x.value === k)?.label ?? k;
const host = (url: string) => {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return null;
  }
};

export function FacultyResourcesScreen() {
  const api = useFlexibleApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const data = useQuery({ queryKey: fkeys.scoped("my", "resources"), queryFn: () => api.myResources() });
  const sections = data.data?.sections ?? [];
  const items = data.data?.items ?? [];
  const drafts = items.filter((r) => r.status === "draft").length;
  const courses = new Set(sections.map((s) => s.offeringId)).size;

  const upload = (preset?: TaughtSection) =>
    ask({
      eyebrow: "Resources · Upload",
      title: "Upload material",
      name: "resource",
      submitLabel: "Upload",
      fields: [
        ...(sections.length > 1 && !preset
          ? [{ name: "sectionId", label: "Class", type: "select" as const, required: true, initial: sections[0]?.sectionId, wide: true, options: sections.map((s) => ({ value: s.sectionId, label: `${courseCode(s.offeringCode)} · ${s.title} · Sec ${s.sectionCode}` })) }]
          : []),
        { name: "title", label: "Title", required: true, wide: true, placeholder: "For example, Unit 3: Stacks and queues" },
        { name: "kind", label: "Type", type: "select", required: true, initial: "reading", options: KINDS },
        { name: "url", label: "Link", required: true, placeholder: "https://…" },
        { name: "description", label: "Note for students", type: "textarea" },
        { name: "publish", label: "Visible to students", type: "toggle", initial: "true", wide: true },
      ],
      onSubmit: (v) => {
        const sectionId = preset?.sectionId ?? v.sectionId ?? sections[0]!.sectionId;
        return submit(() => api.createResource(sectionId, { title: v.title!, kind: v.kind as ResourceKind, url: v.url!, description: v.description || undefined, publish: v.publish === "true" }), "Material uploaded");
      },
    });

  const row = (r: CourseResource) => {
    const look = KIND_LOOK[r.kind] ?? KIND_LOOK.document!;
    const created = (r as CourseResource & { createdAt?: string }).createdAt;
    return (
      <ListRow
        key={r.id}
        data-resource-row={r.title}
        icon={look.icon}
        tone={look.tone}
        title={
          <a href={r.url} target="_blank" rel="noreferrer noopener" data-resource={r.title}>
            {r.title}
          </a>
        }
        meta={[kindLabel(r.kind), host(r.url), created ? formatDateShort(created) : null].filter(Boolean).join(" · ")}
        badge={r.status === "published" ? { label: "Visible to students", kind: "info" } : r.status === "draft" ? { label: "Draft", kind: "neutral" } : { label: "Withdrawn", kind: "neutral" }}
        selected={r.status === "draft"}
        primary={r.status !== "published" ? { label: "Publish", ghost: true, disabled: busy, onClick: () => void run(() => api.resourceCommand(r.id, r.version, "publish"), "Published"), "data-action": "publish-resource" } : undefined}
        menu={[
          { id: "open-resource", label: "Open link", onSelect: () => window.open(r.url, "_blank", "noopener,noreferrer") },
          ...(r.status === "published" ? [{ id: "withdraw-resource", label: "Withdraw from students", danger: true, disabled: busy, onSelect: () => void run(() => api.resourceCommand(r.id, r.version, "withdraw"), "Withdrawn") }] : []),
        ]}
      />
    );
  };

  return (
    <>
      <AreaHeader
        title="Resources"
        summary={`${items.length} ${items.length === 1 ? "material" : "materials"} across ${courses} ${courses === 1 ? "course" : "courses"} · ${drafts} in draft`}
        actions={
          sections.length > 0 ? (
            <button type="button" className="sb-btn" data-new-resource="" onClick={() => upload()}>
              <Upload strokeWidth={1.75} aria-hidden="true" />
              Upload material
            </button>
          ) : undefined
        }
        data-slice="4C"
      />
      {sections.length === 0 ? (
        <BentoTile span={12}>
          <EmptyState icon={BookOpen} title="No classes assigned">
            Resources are added to the sections you actively teach.
          </EmptyState>
        </BentoTile>
      ) : (
        sections.map((s) => {
          const rows = items.filter((r) => r.sectionId === s.sectionId || (!r.sectionId && r.offeringId === s.offeringId));
          return (
            <BentoTile key={s.sectionId} span={12} data-resource-section={s.sectionId}>
              <div className="ac-th">
                <h2>
                  {courseCode(s.offeringCode)} · {s.title} · Sec {s.sectionCode}
                </h2>
                <span className="ac-th-end ac-muted">
                  {rows.length} {rows.length === 1 ? "item" : "items"}
                </span>
              </div>
              {rows.length === 0 ? (
                <EmptyState
                  icon={Library}
                  title="No materials yet"
                  compact
                  action={
                    <button type="button" className="sb-btn sb-btn--ghost" onClick={() => upload(s)}>
                      <ExternalLink strokeWidth={1.75} aria-hidden="true" />
                      Add the first one
                    </button>
                  }
                >
                  Share reading lists, documents, videos or library links.
                </EmptyState>
              ) : (
                <div className="sb-list-rows">{rows.map(row)}</div>
              )}
            </BentoTile>
          );
        })
      )}
      {prompt}
      {dialog}
    </>
  );
}
