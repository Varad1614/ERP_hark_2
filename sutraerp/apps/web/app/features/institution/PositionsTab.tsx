import { Plus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Modal } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { PositionCommand } from "./api";
import { Actions, fmtDate, NoInstitution, QuickForm } from "./forms";
import { keys, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { Institution, Position } from "./types";

const NEXT: Record<Position["status"], PositionCommand[]> = { draft: ["activate"], active: ["retire"], retired: ["archive"], archived: [] };
const LABEL: Record<PositionCommand, string> = { activate: "Activate", retire: "Retire", archive: "Archive" };

export function PositionsTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const [adding, setAdding] = useState(false);
  const inst = institution?.id ?? "-";
  const positions = useQuery({ queryKey: keys.scoped(inst, "positions"), queryFn: () => api.listPositions(institution!.id), enabled: !!institution });
  const units = useQuery({ queryKey: keys.scoped(inst, "org-units"), queryFn: () => api.listOrgUnits(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const unitName = (id: string) => units.data?.find((u) => u.id === id)?.name ?? "—";
  return (
    <>
      <BentoTile
        span={12}
        title="Position definitions"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} disabled={(units.data ?? []).length === 0} data-new-position>
            Define position
          </Button>
        }
      >
        <DataTable
          caption="Positions"
          rows={positions.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState title="No positions yet">{(units.data ?? []).length === 0 ? "Add an organisation unit first." : "Define the first position."}</EmptyState>}
          columns={[
            { key: "code", header: "Code", render: (p) => p.code },
            { key: "title", header: "Title", render: (p) => <span data-position={p.code}>{p.title}</span> },
            { key: "unit", header: "Unit", render: (p) => unitName(p.orgUnitId), muted: true },
            { key: "eff", header: "Effective", render: (p) => `${fmtDate(p.effectiveFrom)}${p.effectiveTo ? ` – ${fmtDate(p.effectiveTo)}` : ""} · rev ${p.revision}`, muted: true },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            {
              key: "actions",
              header: "",
              render: (p) => <Actions disabled={busy} actions={NEXT[p.status].map((a) => ({ id: `position-${a}`, label: LABEL[a], onClick: () => void run(() => api.positionCommand(p.id, p.version, a), `${p.title}: ${LABEL[a].toLowerCase()} done`) }))} />,
            },
          ]}
        />
      </BentoTile>
      <Modal open={adding} onClose={() => setAdding(false)} label="Define position">
        <h2>Define position</h2>
        <QuickForm
          name="new-position"
          submitLabel="Define"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "orgUnitId", label: "Organisation unit", type: "select", initial: units.data?.[0]?.id ?? "", options: (units.data ?? []).map((u) => ({ value: u.id, label: u.name })) },
            { name: "code", label: "Code", required: true, placeholder: "HOD-CSE" },
            { name: "title", label: "Title", required: true, placeholder: "Head of Department" },
            { name: "effectiveFrom", label: "Effective from", type: "date", required: true },
          ]}
          onSubmit={async (v) => {
            await submit(() => api.createPosition(institution.id, { orgUnitId: v.orgUnitId!, code: v.code!.trim().toUpperCase(), title: v.title!.trim(), effectiveFrom: v.effectiveFrom! }), "Position defined");
            setAdding(false);
          }}
        />
      </Modal>
      {dialog}
    </>
  );
}
