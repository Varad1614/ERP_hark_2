import { describe, expect, it } from "vitest";
import { csvCell, csvRow } from "./csv";

describe("csvCell (SEC-P1-APPSEC-006)", () => {
  it("neutralises spreadsheet formulas", () => {
    expect(csvCell('=HYPERLINK("http://evil.example","x")')).toBe(`"'=HYPERLINK(""http://evil.example"",""x"")"`);
    expect(csvCell("=cmd|' /C calc'!A0")).toBe("'=cmd|' /C calc'!A0");
    expect(csvCell("+SUM(A1:A9)")).toBe("'+SUM(A1:A9)");
    expect(csvCell("-2+3")).toBe("'-2+3");
    expect(csvCell("@SUM(A1)")).toBe("'@SUM(A1)");
    expect(csvCell("\t=1+1")).toBe("'\t=1+1");
  });

  it("keeps plain numbers and text", () => {
    expect(csvCell("-1250.50")).toBe("-1250.50");
    expect(csvCell("+5")).toBe("+5");
    expect(csvCell(42)).toBe("42");
    expect(csvCell("Fees collected")).toBe("Fees collected");
    expect(csvCell(null)).toBe("");
  });

  it("quotes separators and quotes", () => {
    expect(csvCell("a,b")).toBe('"a,b"');
    expect(csvCell('say "hi"')).toBe('"say ""hi"""');
    expect(csvRow(["x", "=1", "1,2"])).toBe(`x,'=1,"1,2"`);
  });
});
