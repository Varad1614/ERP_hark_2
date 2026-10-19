import { Construction } from "lucide-react";
import { BentoTile } from "@sutra/ui";

/** Foundation placeholder rendered by every Batch 4 screen / tab until its slice replaces it. */
export function SlicePlaceholder({ title, slice, detail }: { title: string; slice: string; detail?: string }) {
  return (
    <BentoTile span={12} variant="accent" className="sb-soon" data-placeholder={slice}>
      <span className="sb-chip">
        <Construction strokeWidth={1.75} /> Batch 4 · slice {slice}
      </span>
      <h2>{title}</h2>
      <p>{detail ?? "Being built in Batch 4 (Academic delivery)."}</p>
    </BentoTile>
  );
}
