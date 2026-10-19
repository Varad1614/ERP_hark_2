import { CalendarDays, Check, ChevronDown, Plus, Upload, X } from "lucide-react";
import { useId, useState, type InputHTMLAttributes, type KeyboardEvent, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes } from "react";
import { cx } from "./cx";
import { formatDate } from "./format";

export interface FieldShellProps {
  label: string;
  hint?: ReactNode;
  error?: string;
  /** Visually hidden label (cells that already have a column header). */
  hideLabel?: boolean;
  /** Spans every column of a `FormGrid`. */
  wide?: boolean;
  className?: string;
}

/** Label + control + hint/error column (prototype `.x-lbl` above an `.x-in`). */
export function Field({ label, hint, error, hideLabel, wide, className, htmlFor, children }: FieldShellProps & { htmlFor?: string; children: ReactNode }) {
  return (
    <div className={cx("sb-field", wide && "sb-wide", className)}>
      <label htmlFor={htmlFor} className={hideLabel ? "sb-visually-hidden" : undefined}>
        {label}
      </label>
      {children}
      {hint && !error && <span className="sb-field-hint">{hint}</span>}
      {error && (
        <span className="sb-field-error" role="alert">
          {error}
        </span>
      )}
    </div>
  );
}

/** Two- or three-column form grid (prototype `.x-g2` / `.x-lf`); single column under 640 px. */
export function FormGrid({ columns = 2, children, ...rest }: { columns?: 1 | 2 | 3; children: ReactNode; [key: `data-${string}`]: string | undefined }) {
  return (
    <div className="sb-fgrid" style={{ ["--cols" as string]: columns }} {...rest}>
      {children}
    </div>
  );
}

export interface FormSelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/**
 * Styled select (filled `--bg`, radius 16, chevron). Native `<select>` underneath so keyboard,
 * mobile pickers and e2e `selectOption` keep working. `placeholder` adds an empty first option.
 */
export function FormSelect({ label, hint, error, hideLabel, wide, options, placeholder, className, ...rest }: FieldShellProps & { options: FormSelectOption[]; placeholder?: string } & Omit<SelectHTMLAttributes<HTMLSelectElement>, "className">) {
  const id = useId();
  return (
    <Field label={label} hint={hint} error={error} hideLabel={hideLabel} wide={wide} className={className} htmlFor={id}>
      <span className="sb-select-wrap">
        <select id={id} className="sb-select" aria-invalid={error ? true : undefined} {...rest}>
          {placeholder !== undefined && <option value="">{placeholder}</option>}
          {options.map((o) => (
            <option key={o.value} value={o.value} disabled={o.disabled}>
              {o.label}
            </option>
          ))}
        </select>
        <ChevronDown strokeWidth={1.75} aria-hidden="true" />
      </span>
    </Field>
  );
}

/** Text input in the prototype style. */
export function TextField({ label, hint, error, hideLabel, wide, className, ...rest }: FieldShellProps & Omit<InputHTMLAttributes<HTMLInputElement>, "className">) {
  const id = useId();
  return (
    <Field label={label} hint={hint} error={error} hideLabel={hideLabel} wide={wide} className={className} htmlFor={id}>
      <input id={id} className="sb-input" aria-invalid={error ? true : undefined} {...rest} />
    </Field>
  );
}

/** Multi-line text (prototype `.x-ta`). */
export function TextArea({ label, hint, error, hideLabel, wide, className, ...rest }: FieldShellProps & Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, "className">) {
  const id = useId();
  return (
    <Field label={label} hint={hint} error={error} hideLabel={hideLabel} wide={wide} className={className} htmlFor={id}>
      <textarea id={id} className="sb-input sb-textarea" aria-invalid={error ? true : undefined} {...rest} />
    </Field>
  );
}

/**
 * Date field showing `dd MMM yyyy` ("15 Jul 2026") instead of the browser's mm/dd/yyyy. A native
 * `type="date"` input sits invisibly on top, so the platform picker opens on click and the value
 * stays ISO (`2026-07-15`) — forms and e2e `fill("2026-07-15")` are unchanged.
 */
export function DatePicker({ label, hint, error, hideLabel, wide, className, value, defaultValue, onChange, placeholder = "Pick a date", ...rest }: FieldShellProps & { placeholder?: string } & Omit<InputHTMLAttributes<HTMLInputElement>, "className" | "type" | "placeholder">) {
  const id = useId();
  const controlled = value !== undefined;
  const [inner, setInner] = useState(String(defaultValue ?? ""));
  const current = controlled ? String(value ?? "") : inner;
  return (
    <Field label={label} hint={hint} error={error} hideLabel={hideLabel} wide={wide} className={className} htmlFor={id}>
      <span className="sb-date" aria-invalid={error ? true : undefined}>
        <span className={cx("sb-date-text", !current && "is-empty")} aria-hidden="true">
          {current ? formatDate(current) : placeholder}
        </span>
        <CalendarDays strokeWidth={1.75} aria-hidden="true" />
        <input
          id={id}
          type="date"
          value={controlled ? current : undefined}
          defaultValue={controlled ? undefined : defaultValue}
          aria-invalid={error ? true : undefined}
          onChange={(e) => {
            if (!controlled) setInner(e.target.value);
            onChange?.(e);
          }}
          {...rest}
        />
      </span>
    </Field>
  );
}

/**
 * Tags / multi-select (replaces comma-separated text fields). With `options` it is a set of
 * toggle chips; free entry adds a tag on Enter or comma. `name` renders a hidden input with the
 * comma-joined value for plain `<form>` submits.
 */
export function TagInput({ label, hint, error, hideLabel, wide, className, value, onChange, options, allowCustom = true, placeholder = "Type and press Enter", name }: FieldShellProps & { value: string[]; onChange: (next: string[]) => void; options?: FormSelectOption[]; allowCustom?: boolean; placeholder?: string; name?: string }) {
  const id = useId();
  const [draft, setDraft] = useState("");
  const labelOf = (v: string) => options?.find((o) => o.value === v)?.label ?? v;
  const add = (raw: string) => {
    const parts = raw
      .split(",")
      .map((p) => p.trim())
      .filter(Boolean);
    if (parts.length === 0) return;
    const next = [...value];
    for (const p of parts) if (!next.includes(p)) next.push(p);
    onChange(next);
    setDraft("");
  };
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      add(draft);
    } else if (e.key === "Backspace" && !draft && value.length > 0) onChange(value.slice(0, -1));
  };
  const toggle = (v: string) => onChange(value.includes(v) ? value.filter((x) => x !== v) : [...value, v]);
  return (
    <Field label={label} hint={hint} error={error} hideLabel={hideLabel} wide={wide} className={className} htmlFor={allowCustom ? id : undefined}>
      {name && <input type="hidden" name={name} value={value.join(",")} />}
      {(allowCustom || value.some((v) => !options?.some((o) => o.value === v))) && (
        <div className="sb-tags" aria-invalid={error ? true : undefined} data-tags="">
          {value.map((v) => (
            <span key={v} className="sb-tag">
              {labelOf(v)}
              <button type="button" aria-label={`Remove ${labelOf(v)}`} onClick={() => onChange(value.filter((x) => x !== v))}>
                <X strokeWidth={2} />
              </button>
            </span>
          ))}
          {allowCustom && <input id={id} value={draft} placeholder={value.length ? "" : placeholder} onChange={(e) => setDraft(e.target.value)} onKeyDown={onKey} onBlur={() => add(draft)} />}
        </div>
      )}
      {options && options.length > 0 && (
        <div className="sb-tag-options" role="group" aria-label={label}>
          {options.map((o) => {
            const on = value.includes(o.value);
            return (
              <button key={o.value} type="button" className="sb-tag-option" aria-pressed={on} disabled={o.disabled} data-tag-option={o.value} onClick={() => toggle(o.value)}>
                {on ? <Check strokeWidth={2.5} aria-hidden="true" /> : <Plus strokeWidth={2.5} aria-hidden="true" />}
                {o.label}
              </button>
            );
          })}
        </div>
      )}
    </Field>
  );
}

/** Checkbox row (prototype `.x-ck`): filled `--bg` row, accent checkbox, label, optional hint and right note. */
export function Checkbox({ label, description, note, className, disabled, ...rest }: { label: ReactNode; description?: ReactNode; note?: ReactNode; className?: string } & Omit<InputHTMLAttributes<HTMLInputElement>, "type" | "className">) {
  return (
    <label className={cx("sb-ck", disabled && "is-disabled", className)}>
      <input type="checkbox" disabled={disabled} {...rest} />
      <span>
        {label}
        {description && <small>{description}</small>}
      </span>
      {note && <em>{note}</em>}
    </label>
  );
}

/** Column of checkbox rows (replaces bare `<ul>` + native checkbox lists). */
export function CheckboxList({ children, label }: { children: ReactNode; label?: string }) {
  return (
    <div className="sb-checks-col" role="group" aria-label={label}>
      {children}
    </div>
  );
}

/** On/off switch (settings, "Visible to students"). Native checkbox underneath (`role="switch"`). */
export function Toggle({ label, className, ...rest }: { label: ReactNode; className?: string } & Omit<InputHTMLAttributes<HTMLInputElement>, "type" | "className">) {
  return (
    <label className={cx("sb-toggle", className)}>
      <input type="checkbox" role="switch" {...rest} />
      <i aria-hidden="true" />
      <span>{label}</span>
    </label>
  );
}

/** File drop field (prototype `.x-file`): dashed box, turns green once a file is picked. */
export function FileField({ label, hint, fileName, onFile, accept, name }: { label: string; hint?: string; fileName?: string; onFile: (file: File | null) => void; accept?: string; name?: string }) {
  return (
    <label className={cx("sb-file", fileName && "is-ok")}>
      {fileName ? <Check strokeWidth={2} aria-hidden="true" /> : <Upload strokeWidth={1.75} aria-hidden="true" />}
      <span>
        <b>{fileName ?? label}</b>
        {hint && <small>{hint}</small>}
      </span>
      <input type="file" accept={accept} name={name} aria-label={label} onChange={(e) => onFile(e.target.files?.[0] ?? null)} />
    </label>
  );
}
