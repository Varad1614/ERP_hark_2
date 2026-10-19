import type { ReactNode } from "react";
import { cx } from "./cx";
import { SearchField } from "./Input";

export interface ChipOption {
  id: string;
  label: string;
  count?: number;
}

/**
 * One row of filter chips (prototype `.fl` / `.fc`): an optional label column ("Programme"),
 * an "All" chip first, counts in `em`. Single-select; `value` is the selected id.
 */
export function FilterChips({ label, options, value, onChange, search, className, ...rest }: { label?: string; options: ChipOption[]; value: string; onChange: (id: string) => void; search?: { label: string; placeholder?: string; value: string; onChange: (v: string) => void }; className?: string; [key: `data-${string}`]: string | undefined }) {
  return (
    <div className={cx("sb-chip-row", className)} role="group" aria-label={label ?? "Filters"} {...rest}>
      {label && <span className="sb-chip-label">{label}</span>}
      {options.map((o) => (
        <button key={o.id} type="button" className={value === o.id ? "sb-filter on" : "sb-filter"} aria-pressed={value === o.id} data-chip={o.id} onClick={() => onChange(o.id)}>
          {o.label}
          {o.count !== undefined && <em>{o.count.toLocaleString("en-IN")}</em>}
        </button>
      ))}
      {search && <SearchField label={search.label} placeholder={search.placeholder ?? search.label} value={search.value} onChange={(e) => search.onChange(e.target.value)} />}
    </div>
  );
}

/** Stack of labelled chip rows spanning the grid (adm-students: Programme / Semester / Fee status). */
export function FilterChipRows({ children }: { children: ReactNode }) {
  return <div className="sb-chip-rows">{children}</div>;
}
