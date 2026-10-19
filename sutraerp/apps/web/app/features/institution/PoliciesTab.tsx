import { Plus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Modal } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, NoInstitution, QuickForm } from "./forms";
import { Actions } from "../campus/ui";
import { keys, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { Institution } from "./types";

export function PoliciesTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const [adding, setAdding] = useState(false);
  const q = useQuery({ queryKey: keys.scoped(institution?.id ?? "-", "policies"), queryFn: () => api.listPolicies(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  return (
    <>
      <BentoTile
        span={12}
        title="Institutional policies"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-policy>
            Add policy
          </Button>
        }
      >
        <DataTable
          caption="Policies"
          rows={q.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState title="No policies yet" compact>Policies are advisory for activation.</EmptyState>}
          columns={[
            { key: "key", header: "Key", render: (p) => p.key },
            { key: "title", header: "Title", render: (p) => <span data-policy={p.key}>{p.title}</span> },
            { key: "eff", header: "Effective from", render: (p) => `${fmtDate(p.effectiveFrom)} · rev ${p.revision}`, muted: true },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            {
              key: "a",
              header: "",
              render: (p) => <Actions disabled={busy} actions={p.status === "draft" ? [{ id: "policy-publish", label: "Publish", onClick: () => void run(() => api.publishPolicy(p.id, p.version), `${p.title}: published`) }] : []} />,
            },
          ]}
        />
      </BentoTile>
      <Modal open={adding} onClose={() => setAdding(false)} label="Add policy">
        <h2>Add policy</h2>
        <QuickForm
          name="new-policy"
          submitLabel="Add"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "key", label: "Key", required: true, placeholder: "attendance.minimum" },
            { name: "title", label: "Title", required: true, placeholder: "Minimum attendance" },
            { name: "effectiveFrom", label: "Effective from", type: "date", required: true },
            { name: "body", label: "Policy text", required: true, wide: true, placeholder: "Students need 75% attendance per course." },
          ]}
          onSubmit={async (v) => {
            await submit(() => api.createPolicy(institution.id, { key: v.key!.trim(), title: v.title!.trim(), body: v.body!.trim(), effectiveFrom: v.effectiveFrom! }), "Policy added");
            setAdding(false);
          }}
        />
      </Modal>
      {dialog}
    </>
  );
}
