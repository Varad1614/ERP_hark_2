import { describe, expect, it } from "vitest";
import { cellText, fyStart, statementCsv } from "./financial-statements";

describe("financial statements (ERP_EXTENSIONS 1C)", () => {
  it("starts the Indian financial year in April", () => {
    expect(fyStart("2027-03-31")).toBe("2026-04-01");
    expect(fyStart("2026-04-01")).toBe("2026-04-01");
  });
  it("shows percent columns from basis points and blanks for null", () => {
    expect(cellText("-2227.00", true)).toBe("-22.3%");
    expect(cellText(null, false)).toBe("");
  });
  it("exports the rows as served", () => {
    const csv = statementCsv({
      id: "income_expenditure",
      title: "Income & expenditure",
      subtitle: "",
      columns: [{ key: "total", label: "Total" }],
      rows: [
        { kind: "section", key: "Income", label: "Income", depth: 0, values: [null], emphasis: false },
        { kind: "account", label: "Tuition, term 1", depth: 1, values: ["1000.00"], emphasis: false },
      ],
    });
    expect(csv).toBe(',Total\nIncome,\n"  Tuition, term 1",1000.00');
  });
});
