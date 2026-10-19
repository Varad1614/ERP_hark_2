import { BentoTile, EmptyState } from "@sutra/ui";
import { Users } from "lucide-react";

/**
 * Foundation placeholder rendered by every Batch 7 tab / panel until its slice replaces it.
 * Uses only @sutra/ui components (no custom classes).
 */
export function PeoplePlaceholder({ title, slice, detail, span = 12 }: { title: string; slice: string; detail?: string; span?: 4 | 6 | 8 | 12 }) {
  return (
    <BentoTile span={span} title={title} data-placeholder={slice}>
      <EmptyState icon={Users} title={`Batch 7 · slice ${slice}`}>
        {detail ?? "Being built in Batch 7 (People & workforce)."}
      </EmptyState>
    </BentoTile>
  );
}
