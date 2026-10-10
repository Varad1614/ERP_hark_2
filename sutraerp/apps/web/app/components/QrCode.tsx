import { encode } from "uqr";
import { useMemo } from "react";

/** QR code rendered as plain SVG rects (no innerHTML) from the `otpauth://` enrolment URI. */
export function QrCode({ value, size = 184, label }: { value: string; size?: number; label: string }) {
  const { size: n, data } = useMemo(() => encode(value, { ecc: "M", border: 2 }), [value]);
  const cells: string[] = [];
  data.forEach((row, y) =>
    row.forEach((on, x) => {
      if (on) cells.push(`M${x} ${y}h1v1h-1z`);
    }),
  );
  return (
    <svg className="sb-qr" role="img" aria-label={label} width={size} height={size} viewBox={`0 0 ${n} ${n}`} shapeRendering="crispEdges">
      <rect width={n} height={n} fill="#fff" />
      <path d={cells.join("")} fill="currentColor" />
    </svg>
  );
}
