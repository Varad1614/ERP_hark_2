import { Network, Plus } from "lucide-react";
import { BentoTile, Button, EmptyState, IconChip, Modal } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { OrgUnitCommand } from "./api";
import { NoInstitution, QuickForm, todayIso } from "./forms";
import { Actions } from "../campus/ui";
import { keys, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { Institution, OrgUnit, UnitType } from "./types";

export const UNIT_TYPES = ["university", "college", "school", "faculty", "department", "centre", "office"];

/**
 * `university, faculty, department` → blueprint unit types: the first type is the root type;
 * every other type may sit under any listed type (including itself).
 */
export function blueprintTypes(text: string): UnitType[] {
  const keys = [...new Set(text.split(",").map((x) => x.trim().toLowerCase().replace(/[^a-z0-9_]/g, "_")).filter(Boolean))];
  return keys.map((key, i) => ({
    key,
    label: key.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase()),
    allowRoot: i === 0,
    allowedParentTypes: i === 0 ? [] : keys,
  }));
}

/** Depth-first tree order with depth (orphans with an unknown parent become roots). */
export function orgTree(units: OrgUnit[]): { unit: OrgUnit; depth: number }[] {
  const ids = new Set(units.map((u) => u.id));
  const children = new Map<string | null, OrgUnit[]>();
  for (const u of units) {
    const p = u.parentId && ids.has(u.parentId) ? u.parentId : null;
    children.set(p, [...(children.get(p) ?? []), u]);
  }
  const out: { unit: OrgUnit; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const u of (children.get(parent) ?? []).sort((a, b) => a.code.localeCompare(b.code))) {
      out.push({ unit: u, depth });
      walk(u.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

export function orgUnitActions(status: OrgUnit["status"]): OrgUnitCommand[] {
  switch (status) {
    case "draft":
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

const LABEL: Record<OrgUnitCommand, string> = { activate: "Activate", suspend: "Suspend", restore: "Restore", decommission: "Decommission", archive: "Archive" };

export function OrganisationTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const [adding, setAdding] = useState(false);
  const q = useQuery({ queryKey: keys.scoped(institution?.id ?? "-", "org-units"), queryFn: () => api.listOrgUnits(institution!.id), enabled: !!institution });
  const bps = useQuery({ queryKey: keys.scoped(institution?.id ?? "-", "blueprints"), queryFn: () => api.listBlueprints(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const units = q.data ?? [];
  const tree = orgTree(units);
  const blueprint = (bps.data ?? []).find((b) => b.status === "published");
  const types = blueprint?.unitTypes.map((t) => t.key) ?? [];
  return (
    <>
      <BentoTile span={12} title="Unit types">
        {blueprint ? (
          <p data-blueprint={blueprint.versionNo}>
            <small>
              Blueprint v{blueprint.versionNo} (effective {blueprint.effectiveFrom}): {blueprint.unitTypes.map((t) => t.label).join(" → ")}
            </small>
          </p>
        ) : (
          <p>
            <small>Publish the unit types first: organisation units are typed by the effective blueprint.</small>
          </p>
        )}
        <QuickForm
          name="publish-blueprint"
          submitLabel={blueprint ? "Publish new unit types" : "Publish unit types"}
          fields={[
            { name: "types", label: "Unit types (root first, comma separated)", required: true, wide: true, initial: "university, faculty, department" },
            { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
          ]}
          onSubmit={(v) =>
            submit(async () => {
              const draft = await api.createBlueprint(institution.id, { effectiveFrom: v.effectiveFrom!, unitTypes: blueprintTypes(v.types!) });
              return api.publishBlueprint(draft.id, draft.version);
            }, "Unit types published")
          }
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Organisation units"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-org-unit>
            Add unit
          </Button>
        }
      >
        {tree.length === 0 ? (
          <EmptyState icon={Network} title="No organisation units yet" compact>
            Start with the root unit (the institution itself), then add faculties and departments under it.
          </EmptyState>
        ) : (
          <div className="sb-list" data-org-tree>
            {tree.map(({ unit, depth }) => (
              <article key={unit.id} className="sb-list-row" style={{ paddingLeft: `${depth * 1.5}rem` }} data-org-unit={unit.code} data-depth={depth}>
                <IconChip icon={Network} tone={depth === 0 ? "orange" : "blue"} size="sm" />
                <div>
                  <h3>
                    {unit.name} <StatusBadge status={unit.status} />
                  </h3>
                  <small>
                    {unit.code} · {unit.unitType}
                  </small>
                </div>
                <Actions
                  disabled={busy}
                  actions={orgUnitActions(unit.status).map((a) => ({ id: `org-${a}`, label: LABEL[a], onClick: () => void run(() => api.orgUnitCommand(unit.id, unit.version, a), `${unit.name}: ${LABEL[a].toLowerCase()} done`) }))}
                />
              </article>
            ))}
          </div>
        )}
      </BentoTile>
      <Modal open={adding} onClose={() => setAdding(false)} label="Add organisation unit">
        <h2>Add organisation unit</h2>
        <QuickForm
          name="new-org-unit"
          submitLabel="Add"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "parentId", label: "Parent", type: "select", options: [{ value: "", label: "— root unit —" }, ...units.map((u) => ({ value: u.id, label: `${u.name} (${u.code})` }))] },
            { name: "unitType", label: "Type", type: "select", required: true, initial: (units.length === 0 ? types[0] : types[types.length - 1]) ?? "", options: types.map((t) => ({ value: t, label: t })) },
            { name: "code", label: "Code", required: true, placeholder: "CSE" },
            { name: "name", label: "Name", required: true, placeholder: "Computer Science" },
          ]}
          onSubmit={async (v) => {
            await submit(() => api.createOrgUnit(institution.id, { parentId: v.parentId || null, unitType: v.unitType!, code: v.code!.trim().toUpperCase(), name: v.name!.trim() }), "Unit added");
            setAdding(false);
          }}
        />
      </Modal>
      {dialog}
    </>
  );
}
