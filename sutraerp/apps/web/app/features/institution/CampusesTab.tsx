import { Plus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Modal } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { CampusCommand } from "./api";
import { Actions, NoInstitution, QuickForm, REASON, useAsk } from "./forms";
import { keys, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { Campus, Institution } from "./types";

export function campusActions(status: Campus["status"]): CampusCommand[] {
  switch (status) {
    case "registered":
      return ["activate"];
    case "active":
      return ["suspend", "decommission"];
    case "suspended":
      return ["restore", "decommission"];
    case "decommissioned":
      return ["archive"];
    default:
      return [];
  }
}

const LABEL: Record<CampusCommand, string> = { activate: "Activate", suspend: "Suspend", restore: "Restore", decommission: "Decommission", archive: "Archive" };

export function CampusesTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [adding, setAdding] = useState(false);
  const act = (c: Campus, a: CampusCommand) => {
    const done = `${c.name}: ${LABEL[a].toLowerCase()} done`;
    if (a === "suspend")
      return ask({ title: `Suspend ${c.name}`, name: "campus-suspend", submitLabel: "Suspend", fields: [REASON], onSubmit: (v) => submit(() => api.campusCommand(c.id, c.version, a, { reason: v.reason!.trim() }), done) });
    if (a === "decommission")
      return ask({
        title: `Decommission ${c.name}`,
        name: "campus-decommission",
        submitLabel: "Decommission",
        fields: [REASON, { name: "effectiveOn", label: "Effective on", type: "date", required: true }],
        onSubmit: (v) => submit(() => api.campusCommand(c.id, c.version, a, { reason: v.reason!.trim(), effectiveOn: v.effectiveOn! }), done),
      });
    void run(() => api.campusCommand(c.id, c.version, a), done);
  };
  const q = useQuery({ queryKey: keys.scoped(institution?.id ?? "-", "campuses"), queryFn: () => api.listCampuses(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  return (
    <>
      <BentoTile
        span={12}
        title="Campuses"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-campus>
            Register campus
          </Button>
        }
      >
        <DataTable
          caption="Campuses"
          rows={q.data ?? []}
          rowKey={(c) => c.id}
          empty={<EmptyState title="No campuses yet">Register the main campus to continue.</EmptyState>}
          columns={[
            { key: "code", header: "Code", render: (c) => c.code },
            { key: "name", header: "Name", render: (c) => <span data-campus={c.code}>{c.name}{c.isMain ? " · main" : ""}</span> },
            { key: "tz", header: "Timezone", render: (c) => c.timezone ?? `${institution.timezone} (institution)`, muted: true },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            {
              key: "actions",
              header: "",
              render: (c) => (
                <Actions
                  disabled={busy}
                  actions={campusActions(c.status).map((a) => ({ id: `campus-${a}`, label: LABEL[a], onClick: () => act(c, a) }))}
                />
              ),
            },
          ]}
        />
      </BentoTile>
      <Modal open={adding} onClose={() => setAdding(false)} label="Register campus">
        <h2>Register campus</h2>
        <QuickForm
          name="new-campus"
          submitLabel="Register"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "code", label: "Code", required: true, placeholder: "MAIN" },
            { name: "name", label: "Name", required: true, placeholder: "Main campus" },
            { name: "timezone", label: "Timezone override (optional)", placeholder: institution.timezone },
            { name: "line1", label: "Address line 1", required: true, wide: true, placeholder: "1 University Road" },
            { name: "city", label: "City", required: true, placeholder: "Pune" },
            { name: "state", label: "State", required: true, placeholder: "Maharashtra" },
            { name: "postalCode", label: "PIN code", required: true, placeholder: "411007" },
            { name: "isMain", label: "Main campus", type: "checkbox", initial: (q.data ?? []).length === 0 ? "true" : "false" },
          ]}
          onSubmit={async (v) => {
            const isMain = v.isMain === "true";
            await submit(
              () =>
                api.registerCampus(institution.id, {
                  code: v.code!.trim().toUpperCase(),
                  name: v.name!.trim(),
                  campusType: isMain ? "main" : "satellite",
                  isMain,
                  timezone: v.timezone?.trim() || undefined,
                  address: { line1: v.line1!.trim(), city: v.city!.trim(), state: v.state!.trim(), postalCode: v.postalCode!.trim(), country: "IN" },
                }),
              "Campus registered",
            );
            setAdding(false);
          }}
        />
      </Modal>
      {prompt}
      {dialog}
    </>
  );
}
