/** 7-point sparkline of the prototype KPI tiles (`.dspk`): area + line + end dot, coloured by `color`. */
export function Sparkline({ values, color = "var(--ac)", label }: { values: readonly number[]; color?: string; label?: string }) {
  if (values.length < 2) return null;
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const pts = values.map((v, i) => [(i / (values.length - 1)) * 120, 27 - ((v - min) / span) * 22] as const);
  const line = pts.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const last = pts[pts.length - 1]!;
  return (
    <span className="sb-spark-wrap" title={label}>
      <svg className="sb-spark" viewBox="0 0 120 30" preserveAspectRatio="none" aria-hidden="true" style={{ ["--c" as string]: color }}>
        <polygon points={`0,30 ${line} 120,30`} />
        <polyline points={line} />
        <circle cx={last[0].toFixed(1)} cy={last[1].toFixed(1)} r="2.6" />
      </svg>
      {label && <em>{label}</em>}
    </span>
  );
}
