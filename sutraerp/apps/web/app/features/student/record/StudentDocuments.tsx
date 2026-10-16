/**
 * Student-facing Documents (`/documents`), prototype `stu-documents`: header with a live line, an
 * inline "New request" form (document select + purpose + Submit), "Your requests" and the
 * documents on the student's record (`me` endpoints; the API resolves the student from the session).
 */
import { Download, FileText, Send } from "lucide-react";
import { AreaHeader, BentoTile, DataTable, EmptyState, ListRow, StatusBadge, formatDate, humanize, plural } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { ApiProblem } from "@sutra/api-client";
import { Actions, SpecForm } from "../ui";
import { rkeys, useRecordApi, useRecordCommand } from "./hooks";
import { useDownload } from "./files";
import { REQUEST_TYPES } from "./Student360Tabs";

export function StudentDocumentsScreen() {
  const api = useRecordApi();
  const me = useQuery({ queryKey: rkeys.scoped("me"), queryFn: () => api.me(), retry: false });
  const docs = useQuery({ queryKey: rkeys.scoped("me", "documents"), queryFn: () => api.myDocuments(), enabled: !!me.data });
  const requests = useQuery({ queryKey: rkeys.scoped("me", "requests"), queryFn: () => api.myRequests(), enabled: !!me.data });
  const { run, submit, busy, dialog } = useRecordCommand();
  const files = useDownload();
  if (me.isLoading)
    return (
      <AreaHeader title="Documents" summary="Loading…" />
    );
  if (!me.data) {
    const missing = me.error instanceof ApiProblem && me.error.problem.status === 404;
    return (
      <>
        <AreaHeader title="Documents" summary="Request certificates from the registrar’s office" />
        <BentoTile span={12} data-no-student>
          <EmptyState icon={FileText} title={missing ? "No student record linked" : "Documents unavailable"}>
            {missing ? "Your account is not linked to a student record yet. Contact the admissions office." : "Try again in a moment."}
          </EmptyState>
        </BentoTile>
      </>
    );
  }
  const s = me.data;
  const list = requests.data ?? [];
  const ready = list.filter((r) => r.status === "issued").length;
  const open = list.filter((r) => r.status === "requested" || r.status === "in_progress").length;
  const summary = list.length
    ? [open ? `${plural(open, "request")} in progress` : null, ready ? `${ready} issued` : null, `${plural((docs.data ?? []).length, "document")} on your record`].filter(Boolean).join(" · ")
    : "Request certificates from the registrar’s office";
  return (
    <>
      <AreaHeader title="Documents" summary={<span data-student-number={s.studentNumber}>{summary}</span>} />
      <BentoTile span={12} title="New request">
        <SpecForm
          name="my-request"
          submitLabel="Submit request"
          inline={{ icon: <Send strokeWidth={1.75} aria-hidden="true" /> }}
          fields={[
            { name: "documentType", label: "Document", type: "select", initial: REQUEST_TYPES[0], options: REQUEST_TYPES.map((t) => ({ value: t, label: humanize(t) })) },
            { name: "purpose", label: "Purpose", placeholder: "For example, passport application" },
          ]}
          onSubmit={(v) => submit(() => api.raiseMyRequest({ documentType: v.documentType!, purpose: v.purpose || null }), "Request submitted")}
        />
      </BentoTile>
      <BentoTile span={12} title="Your requests" action={<span className="st-count">{`${list.length} total`}</span>}>
        <DataTable
          caption="My requests"
          rows={list}
          rowKey={(r) => r.id}
          empty={
            <EmptyState title="No requests yet" compact>
              Request a bonafide certificate, transcript copy and more.
            </EmptyState>
          }
          columns={[
            {
              key: "type",
              header: "Document",
              render: (r) => (
                <span data-my-request={r.documentType}>
                  {humanize(r.documentType)}
                  {r.purpose && <small style={{ display: "block", color: "var(--sb-mute-2)", fontSize: 12.5 }}>{r.purpose}</small>}
                </span>
              ),
            },
            { key: "when", header: "Requested", render: (r) => formatDate(r.createdAt.slice(0, 10)), muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} labels={{ issued: "Ready to collect" }} /> },
            { key: "note", header: "Reference", render: (r) => r.issuedReference ?? r.statusReason ?? "—", muted: true },
            {
              key: "actions",
              header: "",
              render: (r) => (r.status === "requested" ? <Actions disabled={busy} actions={[{ id: "my-request-cancel", label: "Cancel", onClick: () => void run(() => api.cancelMyRequest(r.id, r.version), "Request cancelled") }]} /> : null),
            },
          ]}
        />
      </BentoTile>
      {(docs.data ?? []).length > 0 && (
        <BentoTile span={12} title="Documents on your record">
          <div className="st-lrows">
            {(docs.data ?? []).map((d) => (
              <ListRow
                key={d.id}
                data-my-document={d.documentType}
                data-file={d.documentObjectId ? "attached" : "none"}
                icon={FileText}
                tone={d.status === "verified" ? "green" : "amber"}
                title={humanize(d.documentType)}
                meta={[d.title, d.documentObjectId ? "File on record" : null].filter(Boolean).join(" · ")}
                badge={{ label: humanize(d.status), kind: d.status === "verified" ? "done" : d.status === "rejected" ? "attention" : "review" }}
                primary={d.documentObjectId ? { label: "Download", icon: Download, ghost: true, disabled: files.busy, "data-action": "my-document-download", onClick: () => void files.download(() => api.downloadMyDocumentFile(d.id)) } : undefined}
              />
            ))}
          </div>
        </BentoTile>
      )}
      {dialog}
    </>
  );
}
