/**
 * Re-skin v2 (R3): student Assignments (prototype `stu-assignments.png`, `-tab-submitted.png`,
 * `-tab-graded.png`): header tabs Pending · n / Submitted · n / Graded · n, `ListRow`s (pencil
 * chip, course · code · due, Due soon / Pending badge, Submit pill) and a submit `StepDialog`.
 * Same API call (`POST /me/assignments/{id}/submission`, idempotent).
 */
import { CheckCircle2, ClipboardCheck, PencilLine, Upload } from "lucide-react";
import { AreaHeader, BentoTile, EmptyState, formatDateTime, formatNumber, ListRow, type BadgeKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useCommand } from "../../institution/hooks";
import { dkeys, useDeliveryApi } from "../delivery/hooks";
import type { MyAssignment } from "../delivery/types";
import { courseCode, toneOf, useAsk } from "../kit";

type Tab = "pending" | "submitted" | "graded";
const DAY_MS = 86400000;

const bucket = (a: MyAssignment): Tab => (a.submission ? (a.submission.marks !== null && a.submission.marks !== undefined ? "graded" : "submitted") : "pending");
const due = (a: MyAssignment) => a.extendedDueAt ?? a.dueAt;

function pendingBadge(a: MyAssignment, now: number): { label: string; kind: BadgeKind } {
  if (a.status === "closed" || a.status === "cancelled") return { label: "Closed", kind: "neutral" };
  if (a.status === "scheduled") return { label: "Not open yet", kind: "neutral" };
  const left = new Date(due(a)).getTime() - now;
  if (left < 0) return { label: "Overdue", kind: "attention" };
  if (left < 2 * DAY_MS) return { label: "Due soon", kind: "attention" };
  return { label: a.extendedDueAt ? "Extended" : "Pending", kind: "neutral" };
}

export function StudentAssignmentsScreen() {
  const api = useDeliveryApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [tab, setTab] = useState<Tab>("pending");
  const q = useQuery({ queryKey: dkeys.scoped("me", "assignments"), queryFn: () => api.myAssignments() });
  const all = q.data ?? [];
  const by = (t: Tab) => all.filter((a) => bucket(a) === t).sort((x, y) => due(x).localeCompare(due(y)));
  const counts = { pending: by("pending").length, submitted: by("submitted").length, graded: by("graded").length };
  const rows = by(tab);
  const now = Date.now();

  const openSubmit = (a: MyAssignment) =>
    ask({
      eyebrow: `${a.courseTitle} · ${courseCode(a.offeringCode)}`,
      title: `Submit · ${a.title}`,
      name: "assignment-submit",
      submitLabel: "Submit",
      intro: <p className="ac-ask-note">{`Due ${formatDateTime(due(a))} · ${formatNumber(a.maxMarks)} marks${a.instructions ? ` · ${a.instructions}` : ""}`}</p>,
      fields: [{ name: "content", label: "Your answer or a link to your work", type: "textarea", required: true, placeholder: "Paste your answer, or a link to a document or repository" }],
      onSubmit: (v) => submit(() => api.submitAssignment(a.id, v.content!), "Submitted"),
    });

  return (
    <>
      <AreaHeader
        title="Assignments"
        summary={`${counts.pending} pending · ${counts.submitted} submitted · ${counts.graded} graded`}
        tabs={[
          { id: "pending", label: "Pending", count: counts.pending },
          { id: "submitted", label: "Submitted", count: counts.submitted },
          { id: "graded", label: "Graded", count: counts.graded },
        ]}
        activeTab={tab}
        onTabChange={(t) => setTab(t as Tab)}
        data-slice="4B"
        data-assignments=""
      />
      <BentoTile span={12} data-assignment-list={tab}>
        {rows.length === 0 ? (
          <EmptyState icon={tab === "graded" ? CheckCircle2 : ClipboardCheck} title={tab === "pending" ? "Nothing pending" : tab === "submitted" ? "Nothing awaiting a grade" : "No graded work yet"} compact>
            {tab === "pending" ? "Open coursework of your courses appears here." : tab === "submitted" ? "Submitted work waits here until it is marked." : "Marks appear here once your faculty grades your work."}
          </EmptyState>
        ) : (
          <div className="sb-list-rows">
            {rows.map((a) => {
              const canSubmit = !a.submission && (a.status === "open" || a.status === "extended");
              const meta = `${a.courseTitle} · ${courseCode(a.offeringCode)} · ${tab === "pending" ? "Due" : "Submitted"} ${formatDateTime(tab === "pending" ? due(a) : (a.submission?.submittedAt ?? due(a)))}`;
              return (
                <ListRow
                  key={a.id}
                  data-assignment={a.id}
                  data-assignment-title={a.title}
                  icon={tab === "graded" ? CheckCircle2 : PencilLine}
                  tone={tab === "graded" ? "green" : toneOf(a.offeringCode)}
                  title={a.title}
                  meta={a.submission?.feedback && tab === "graded" ? `${meta} · “${a.submission.feedback}”` : meta}
                  value={tab === "graded" ? `${formatNumber(a.submission?.marks ?? 0)} / ${formatNumber(a.maxMarks)}` : undefined}
                  badge={tab === "pending" ? pendingBadge(a, now) : tab === "submitted" ? { label: "Submitted", kind: "info" } : { label: "Graded", kind: "done" }}
                  primary={canSubmit ? { label: "Submit", icon: Upload, disabled: busy, onClick: () => openSubmit(a), "data-action": "assignment-submit" } : undefined}
                />
              );
            })}
          </div>
        )}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}
