/**
 * Student-facing Documents (`/documents`): own documents on record and own document /
 * certificate requests (`me` endpoints; the API resolves the student from the session).
 */
import { FileText } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { ApiProblem } from "@sutra/api-client";
import { Actions, fmtDate, useAsk } from "../../institution/forms";
import { StatusBadge } from "../../institution/hooks";
import { rkeys, useRecordApi, useRecordCommand } from "./hooks";
import { humanise, REQUEST_TYPES } from "./Student360Tabs";

export function StudentDocumentsScreen() {
  const api = useRecordApi();
  const me = useQuery({ queryKey: rkeys.scoped("me"), queryFn: () => api.me(), retry: false });
  const docs = useQuery({ queryKey: rkeys.scoped("me", "documents"), queryFn: () => api.myDocuments(), enabled: !!me.data });
  const requests = useQuery({ queryKey: rkeys.scoped("me", "requests"), queryFn: () => api.myRequests(), enabled: !!me.data });
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  if (me.isLoading) return <BentoTile span={12}><p>Loading…</p></BentoTile>;
  if (!me.data) {
    const missing = me.error instanceof ApiProblem && me.error.problem.status === 404;
    return (
      <BentoTile span={12} variant="accent" data-no-student>
        <h1>My documents</h1>
        <EmptyState icon={FileText} title={missing ? "No student record linked" : "Documents unavailable"}>
          {missing ? "Your account is not linked to a student record yet. Contact the admissions office." : "Try again in a moment."}
        </EmptyState>
      </BentoTile>
    );
  }
  const s = me.data;
  return (
    <>
      <BentoTile span={12} variant="accent">
        <h1>My documents</h1>
        <p data-student-number={s.studentNumber}>
          {s.displayName} · {s.studentNumber} {s.currentContext && <StatusBadge status={s.currentContext.status} />}
        </p>
      </BentoTile>
      <BentoTile
        span={12}
        title="Certificate requests"
        action={
          <Button
            size="sm"
            data-request-certificate
            onClick={() =>
              ask({
                title: "Request a document",
                name: "my-request",
                submitLabel: "Request",
                fields: [
                  { name: "documentType", label: "Document", type: "select", initial: REQUEST_TYPES[0], options: REQUEST_TYPES.map((t) => ({ value: t, label: humanise(t) })) },
                  { name: "purpose", label: "Purpose", wide: true },
                ],
                onSubmit: (v) => submit(() => api.raiseMyRequest({ documentType: v.documentType!, purpose: v.purpose || null }), "Request submitted"),
              })
            }
          >
            Request a document
          </Button>
        }
      >
        <DataTable
          caption="My requests"
          rows={requests.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState title="No requests yet">Request a bonafide certificate, transcript copy and more.</EmptyState>}
          columns={[
            { key: "type", header: "Document", render: (r) => <span data-my-request={r.documentType}>{humanise(r.documentType)}</span> },
            { key: "when", header: "Requested", render: (r) => fmtDate(r.createdAt.slice(0, 10)), muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "note", header: "Note", render: (r) => r.issuedReference ?? r.statusReason ?? "—", muted: true },
            {
              key: "actions",
              header: "",
              render: (r) =>
                r.status === "requested" ? <Actions disabled={busy} actions={[{ id: "my-request-cancel", label: "Cancel", onClick: () => void run(() => api.cancelMyRequest(r.id, r.version), "Request cancelled") }]} /> : null,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Documents on my record">
        <DataTable
          caption="My documents"
          rows={docs.data ?? []}
          rowKey={(d) => d.id}
          empty={<EmptyState title="No documents yet">Verified admission documents and issued certificates appear here.</EmptyState>}
          columns={[
            { key: "type", header: "Document", render: (d) => humanise(d.documentType) },
            { key: "title", header: "Title", render: (d) => d.title, muted: true },
            { key: "status", header: "Status", render: (d) => <StatusBadge status={d.status} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}
