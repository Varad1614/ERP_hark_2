import { CircleCheck, CircleDashed, CircleMinus, RefreshCw } from "lucide-react";
import { BentoTile, Button, EmptyState, IconChip } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";
import { NoInstitution } from "./forms";
import { keys, problemText, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { Institution, ReadinessArea, ReadinessItem } from "./types";

const AREAS: { key: ReadinessArea; label: string }[] = [
  { key: "institution", label: "Institution" },
  { key: "organisation", label: "Organisation" },
  { key: "campus", label: "Campuses" },
  { key: "curriculum", label: "Curriculum" },
  { key: "calendar", label: "Academic calendar" },
];

/** Groups checklist items by area (areas without items are dropped). */
export function groupReadiness(items: ReadinessItem[]) {
  return AREAS.map((a) => ({ ...a, items: items.filter((i) => i.area === a.key) })).filter((g) => g.items.length > 0);
}

function Icon({ item }: { item: ReadinessItem }) {
  if (item.status === "met") return <IconChip icon={CircleCheck} tone="green" size="sm" />;
  if (item.status === "not_applicable") return <IconChip icon={CircleMinus} tone="blue" size="sm" />;
  return <IconChip icon={CircleDashed} tone={item.required ? "orange" : "amber"} size="sm" />;
}

export function ReadinessTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, busy, dialog } = useCommand();
  const q = useQuery({
    queryKey: keys.scoped(institution?.id ?? "-", "readiness"),
    queryFn: () => api.readiness(institution!.id),
    enabled: !!institution,
  });
  if (!institution) {
    return (
      <BentoTile span={12}>
        <NoInstitution />
      </BentoTile>
    );
  }
  const r = q.data;
  const blocking = r?.items.filter((i) => i.required && i.status === "unmet").length ?? 0;
  return (
    <>
      <BentoTile
        span={12}
        title="Activation readiness"
        action={
          <Button size="sm" variant="ghost" icon={<RefreshCw strokeWidth={1.75} />} onClick={() => void q.refetch()} data-refresh-readiness>
            Refresh
          </Button>
        }
      >
        {q.isLoading ? (
          <div className="sb-skeleton" style={{ width: "50%" }} />
        ) : q.isError || !r ? (
          <EmptyState icon={CircleDashed} title="Readiness is unavailable">
            {problemText(q.error)}
          </EmptyState>
        ) : (
          <div data-readiness data-ready={String(r.ready)}>
            <p>
              {r.ready ? (
                <>
                  <StatusBadge status="met" /> Every required item is met — the institution can be submitted for activation.
                </>
              ) : (
                <>
                  <StatusBadge status="unmet" /> {blocking} required item{blocking === 1 ? "" : "s"} still open.
                </>
              )}
            </p>
            {groupReadiness(r.items).map((g) => (
              <section key={g.key} data-readiness-area={g.key}>
                <h3>{g.label}</h3>
                <div className="sb-list">
                  {g.items.map((i) => (
                    <article key={i.key} className={`sb-list-row${i.status === "unmet" ? "" : " is-muted"}`} data-readiness-item={i.key} data-status={i.status}>
                      <Icon item={i} />
                      <div>
                        <h3>
                          {i.label} {!i.required && <small>(advisory)</small>}
                        </h3>
                        {i.detail && <p>{i.detail}</p>}
                      </div>
                      <div className="sb-row-actions">
                        <StatusBadge status={i.status} />
                        {i.status === "unmet" && i.link && <Link to={i.link}>Fix</Link>}
                      </div>
                    </article>
                  ))}
                </div>
              </section>
            ))}
            {r.ready && institution.status === "configured" && (
              <div className="sb-actions-row">
                <Button disabled={busy} onClick={() => void run(() => api.institutionCommand(institution.id, institution.version, "submit"), "Submitted for activation")} data-action="submit">
                  Submit for activation
                </Button>
              </div>
            )}
          </div>
        )}
      </BentoTile>
      {dialog}
    </>
  );
}
