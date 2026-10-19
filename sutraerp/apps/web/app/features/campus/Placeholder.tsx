import { BentoTile, EmptyState } from "@sutra/ui";
import { Building2 } from "lucide-react";

/**
 * Foundation placeholder rendered by every Batch 8 tab / panel until its slice replaces it.
 * Uses only @sutra/ui components (no custom classes).
 */
export function CampusPlaceholder({ title, slice, detail, span = 12 }: { title: string; slice: string; detail?: string; span?: 4 | 6 | 8 | 12 }) {
  return (
    <BentoTile span={span} title={title} data-placeholder={slice}>
      <EmptyState icon={Building2} title={`Batch 8 · slice ${slice}`}>
        {detail ?? "Being built in Batch 8 (Campus · Services · Resources)."}
      </EmptyState>
    </BentoTile>
  );
}
