/**
 * Re-skin v2 (R7) — Admin → Reports → Overview (`/reports`), the prototype Reports page
 * (`adm-reports.png` + `adm-reports-dlg-a2-rpt.png`): standard report cards with a sized mini chart
 * (no oversized arrow), each opening a chart dialog with the figures and a CSV export.
 *
 * Figures are the institution's live operational metrics (`GET /services/institutions/{id}/metrics`,
 * every domain's `OperationalMetricsPort`). The API returns current values, not time series, so the
 * cards compare a domain's figures side by side instead of drawing invented trends.
 */
import { BookOpen, Building2, GraduationCap, Package, Users, Wallet, type LucideIcon } from "lucide-react";
import { BarChart, BentoTile, DataTable, EmptyState, ReportCard, StepDialog, formatINR, formatNumber, type Tone } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { akeys, useAnalyticsApi } from "./analytics/hooks";
import type { MetricValue } from "./analytics/types";
import { csvRow } from "../../lib/csv";

interface ReportDef {
  id: string;
  title: string;
  description: string;
  icon: LucideIcon;
  tone: Tone;
  match: (m: MetricValue) => boolean;
}

/** The standard reports: one per owning domain (campus split into spaces / library). */
export const STANDARD_REPORTS: ReportDef[] = [
  { id: "students", title: "Enrolment", description: "Students on roll, by status", icon: GraduationCap, tone: "orange", match: (m) => m.domain === "student" },
  { id: "people", title: "Faculty & staff", description: "Working and teaching staff", icon: Users, tone: "blue", match: (m) => m.domain === "people" },
  { id: "finance", title: "Fee collection", description: "Collected and outstanding, ₹", icon: Wallet, tone: "green", match: (m) => m.domain === "finance" },
  { id: "campus", title: "Campus & service desk", description: "Spaces, seating and open tickets", icon: Building2, tone: "teal", match: (m) => m.domain === "campus" && !m.key.startsWith("campus.library") },
  { id: "library", title: "Library", description: "Members, titles, loans and fines", icon: BookOpen, tone: "violet", match: (m) => m.key.startsWith("campus.library") },
  { id: "resources", title: "Resources", description: "Items, stock and payables", icon: Package, tone: "amber", match: (m) => m.domain === "resources" },
];

const isMoney = (m: MetricValue) => m.unit === "inr";
export const metricText = (m: MetricValue) => (isMoney(m) ? formatINR(m.value, { compact: true }) : formatNumber(m.value));
const num = (m: MetricValue) => Number(m.value) || 0;

/** The figures a chart compares: those sharing the first figure's unit (₹ with ₹, counts with counts). */
function chartable(ms: MetricValue[]): MetricValue[] {
  const first = ms[0];
  return first ? ms.filter((m) => isMoney(m) === isMoney(first)) : [];
}

/** Sized mini chart of a card: horizontal bars, longest first (prototype admissions-funnel style). */
function MiniBars({ figures }: { figures: MetricValue[] }) {
  const rows = chartable(figures).slice(0, 5);
  const max = Math.max(1, ...rows.map(num));
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }} aria-hidden="true">
      {rows.map((m, i) => (
        <div key={m.key} style={{ height: 12, width: `${Math.max(6, (num(m) / max) * 100)}%`, borderRadius: 6, background: i === 0 ? "var(--ac)" : "color-mix(in srgb, var(--ac) 45%, transparent)" }} />
      ))}
    </div>
  );
}

/** CSV of a report's figures (label, key, value, unit). */
export function reportCsv(title: string, figures: MetricValue[]): string {
  // SEC-P1-APPSEC-006: quoting + spreadsheet formula neutralisation (lib/csv).
  return [csvRow(["Report", "Figure", "Key", "Value", "Unit"]), ...figures.map((m) => csvRow([title, m.label, m.key, m.value, m.unit ?? ""]))].join("\n") + "\n";
}

function download(name: string, csv: string) {
  const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function ReportsOverview({ institutionId }: { institutionId: string }) {
  const api = useAnalyticsApi();
  const q = useQuery({ queryKey: akeys(institutionId, "metrics"), queryFn: () => api.metrics(institutionId) });
  const metrics = q.data ?? [];
  const [open, setOpen] = useState<string | null>(null);
  const reports = STANDARD_REPORTS.map((r) => ({ ...r, figures: metrics.filter(r.match) })).filter((r) => r.figures.length > 0);
  const current = reports.find((r) => r.id === open);
  if (!q.isLoading && reports.length === 0)
    return (
      <BentoTile span={12} title="Standard reports" data-slice="8C">
        <EmptyState icon={GraduationCap} title="No figures reported yet">
          Each area reports its figures here once it holds data.
        </EmptyState>
      </BentoTile>
    );
  return (
    <>
      {reports.map((r) => {
        const lead = r.figures[0]!;
        return (
          <ReportCard
            key={r.id}
            icon={r.icon}
            tone={r.tone}
            title={r.title}
            description={r.description}
            onClick={() => setOpen(r.id)}
            data-slice="8C"
            data-report={r.id}
            chart={
              <>
                <MiniBars figures={r.figures} />
                <p style={{ margin: "14px 0 0", fontSize: 13, color: "var(--sb-mute)" }}>
                  {lead.label} {metricText(lead)}
                </p>
              </>
            }
          />
        );
      })}
      {current && (
        <StepDialog
          open
          onClose={() => setOpen(null)}
          eyebrow={`Reports · ${current.title}`}
          title={current.title}
          primary={{ label: "Download CSV", onClick: () => download(`${current.id}-report.csv`, reportCsv(current.title, current.figures)) }}
          cancelLabel="Close"
          data-report-dialog={current.id}
        >
          <BarChart label={current.title} height={200} data={chartable(current.figures).map((m) => ({ label: m.label, value: num(m), display: metricText(m) }))} note={current.description} />
          <DataTable<MetricValue>
            caption={`${current.title} figures`}
            rows={current.figures}
            rowKey={(m) => m.key}
            columns={[
              { key: "label", header: "Figure", render: (m) => m.label },
              { key: "value", header: "Value", render: (m) => (isMoney(m) ? formatINR(m.value) : formatNumber(m.value)) },
            ]}
          />
        </StepDialog>
      )}
    </>
  );
}
