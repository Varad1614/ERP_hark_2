import { BentoTile, EmptyState } from "@sutra/ui";
import { ClipboardCheck } from "lucide-react";

/**
 * Foundation placeholder rendered by every Batch 6 tab / panel until its slice replaces it.
 * Uses only @sutra/ui components (no custom classes) so the re-skin restyles it.
 */
export function ExamsPlaceholder({ title, slice, detail, span = 12 }: { title: string; slice: string; detail?: string; span?: 4 | 6 | 8 | 12 }) {
  return (
    <BentoTile span={span} title={title} data-placeholder={slice}>
      <EmptyState icon={ClipboardCheck} title={`Batch 6 · slice ${slice}`}>
        {detail ?? "Being built in Batch 6 (Examinations)."}
      </EmptyState>
    </BentoTile>
  );
}
