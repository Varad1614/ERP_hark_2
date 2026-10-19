import { Construction } from "lucide-react";
import { BentoTile } from "@sutra/ui";

/** Foundation placeholder rendered by every Batch 5 tab / panel until its slice replaces it. */
export function FinancePlaceholder({ title, slice, detail, span = 12 }: { title: string; slice: string; detail?: string; span?: 4 | 6 | 8 | 12 }) {
  return (
    <BentoTile span={span} variant="accent" className="sb-soon" data-placeholder={slice}>
      <span className="sb-chip">
        <Construction strokeWidth={1.75} /> Batch 5 · slice {slice}
      </span>
      <h2>{title}</h2>
      <p>{detail ?? "Being built in Batch 5 (Finance)."}</p>
    </BentoTile>
  );
}
