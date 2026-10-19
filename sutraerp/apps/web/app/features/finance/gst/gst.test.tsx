/** ERP_EXTENSIONS 5D: GST labels, the profile form shaping, the HTTP layer and the GST tile. */
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderScreen } from "../../../test/harness";
import { createHttpGstApi, createMockGstApi, csvFileName, dispositionName, GST_PATHS } from "./api";
import { ChargeGstSection } from "./ChargeGstSection";
import { defaultPeriod, gkeys, GSTIN_SHAPE, noteLabel, normalGstin, profileInput, taxLabel } from "./gst";
import { GstTile } from "./GstTile";
import type { GstFeeHeadTax, GstTaxDocument } from "./types";

const tax = (p: Partial<GstFeeHeadTax>): GstFeeHeadTax => ({ sac: "996311", gstRate: "12", exempt: false, treatment: "taxable", taxNote: "", taxInclusive: false, version: 1, ...p });

describe("5D GST labels", () => {
  it("splits an intra-state rate into CGST + SGST halves and names IGST across states", () => {
    expect(taxLabel(tax({ gstRate: "18" }))).toBe("18% · CGST 9% + SGST 9%");
    expect(taxLabel(tax({ gstRate: "5" }))).toBe("5% · CGST 2.5% + SGST 2.5%");
    expect(taxLabel(tax({ gstRate: "12" }), true)).toBe("12% IGST");
  });

  it("calls exempt, nil-rated and missing set-ups what they are", () => {
    expect(taxLabel(tax({ gstRate: "0", exempt: true, treatment: "exempt" }))).toBe("Exempt");
    expect(taxLabel(tax({ gstRate: "0", treatment: "nil" }))).toBe("Nil-rated (0%)");
    expect(taxLabel(null)).toBe("Not set up");
  });

  it("shows a preset note by its label and own words verbatim", () => {
    const presets = [{ id: "preset:educationExempt", label: "Education services to students (exempt)", text: "…" }];
    expect(noteLabel(presets, "preset:educationExempt")).toBe("Education services to students (exempt)");
    expect(noteLabel(presets, "Supply by a charitable trust")).toBe("Supply by a charitable trust");
    expect(noteLabel(presets, "  ")).toBe("—");
  });

  it("keeps every query key under `institution` so writes refresh it", () => {
    expect(gkeys.scoped("i1", "gst-profile")[0]).toBe("institution");
  });

  it("defaults the period to this month so far", () => {
    expect(defaultPeriod(new Date(2026, 9, 19))).toEqual({ from: "2026-10-01", to: "2026-10-19" });
    expect(defaultPeriod(new Date(2027, 0, 1))).toEqual({ from: "2027-01-01", to: "2027-01-01" });
  });
});

describe("5D profile form", () => {
  it("upper-cases the GSTIN, drops blanks and keeps the series only when given", () => {
    const body = profileInput({ registrationType: "regular", gstin: " 27aapfu0939f1zv ", legalName: " Sutra Institute of Technology ", stateCode: "", defaultTaxNote: "preset:educationExempt", invoicePrefix: "SIT/{FY}/", billPrefix: " " });
    expect(body).toMatchObject({ registrationType: "regular", gstin: "27AAPFU0939F1ZV", legalName: "Sutra Institute of Technology", stateCode: null, invoicePrefix: "SIT/{FY}/", enabled: true });
    expect(body).not.toHaveProperty("billPrefix");
    expect(GSTIN_SHAPE.test(body.gstin!)).toBe(true);
    expect(normalGstin("")).toBeNull();
  });
});

describe("5D HTTP layer", () => {
  it("reads the CSV raw with its file name and treats a missing tax document as none", async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    const fetchImpl = vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      if (url.includes("/gst-summary/csv")) return new Response("GST summary,27AAPFU0939F1ZV\r\n", { status: 200, headers: { "content-type": "text/csv", "content-disposition": 'attachment; filename="gst-summary-2026-10-01-to-2026-10-19.csv"' } });
      if (url.includes("/tax-document")) return new Response(JSON.stringify({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" }), { status: 404 });
      return new Response(null, { status: 204 });
    });
    const api = createHttpGstApi(fetchImpl, "");
    const csv = await api.summaryCsv("i1", { from: "2026-10-01", to: "2026-10-19" });
    expect(csv.fileName).toBe("gst-summary-2026-10-01-to-2026-10-19.csv");
    expect(csv.text.startsWith("GST summary")).toBe(true);
    expect(calls[0]!.url).toBe("/api/v1/finance/institutions/i1/gst-summary/csv?from=2026-10-01&to=2026-10-19");
    expect(await api.chargeDocument("c1")).toBeNull();
    await api.clearFeeHead("i1", "fh1");
    expect(calls[2]!.url).toBe(GST_PATHS.feeHead.replace("{id}", "i1").replace("{feeHeadId}", "fh1"));
    expect(calls[2]!.init.method).toBe("DELETE");
    expect(calls[2]!.init.body).toBeUndefined();
  });

  it("falls back to the server's own file name pattern", () => {
    expect(dispositionName(null, csvFileName({ from: "2026-04-01", to: "2026-04-30" }))).toBe("gst-summary-2026-04-01-to-2026-04-30.csv");
    expect(dispositionName('attachment; filename="x.csv"', "y")).toBe("x.csv");
  });
});

describe("5D GST tile", () => {
  it("shows the default profile, then the fee-head tax table", async () => {
    await renderScreen(<GstTile institutionId="i1" api={createMockGstApi()} />);
    await screen.findByText(/No GST profile yet/);
    expect(document.querySelector('[data-gst-profile="default"]')).not.toBeNull();
    fireEvent.click(document.querySelector('[data-gst-view="heads"]')!);
    await screen.findByText("12% · CGST 6% + SGST 6%");
    expect(screen.getByText("Not set up")).toBeTruthy();
  });

  it("shows the tax breakup of a charge's tax invoice", async () => {
    const api = createMockGstApi();
    const doc: GstTaxDocument = {
      id: "d1", kind: "tax_invoice", title: "Tax Invoice", status: "issued", number: "TI/26-27/0001", date: "2026-10-19", dateDisplay: "19/10/2026", financialYear: "2026-27",
      chargeId: "c1", chargeNumber: "CHG-000002", adjustmentNumber: null,
      supplier: { legalName: "Sutra Institute", tradeName: "", gstin: "27AAPFU0939F1ZV", address: "", stateCode: "27", stateName: "Maharashtra" },
      recipient: { name: "Aarav Desai", studentNumber: "S2026-0001", gstin: null, stateCode: "27", stateName: "Maharashtra" },
      placeOfSupply: "27-Maharashtra", interState: false, reverseCharge: false, lines: [],
      taxes: [{ component: "cgst", label: "CGST @ 6%", rate: "6", amount: "600.00" }, { component: "sgst", label: "SGST @ 6%", rate: "6", amount: "600.00" }],
      taxableValue: "10000.00", cgst: "600.00", sgst: "600.00", igst: "0.00", totalTax: "1200.00", total: "11200.00", totalDisplay: "₹11,200.00",
      amountInWords: "Rupees Eleven Thousand Two Hundred Only", taxNote: "", original: null,
    };
    api.chargeDocument = async () => doc;
    await renderScreen(<ChargeGstSection chargeId="c1" studentAccountId="sa1" institutionId="i1" api={api} />);
    await screen.findByText("TI/26-27/0001");
    expect(screen.getByText("CGST @ 6%")).toBeTruthy();
    expect(screen.getByText("Rupees Eleven Thousand Two Hundred Only")).toBeTruthy();
    await waitFor(() => expect(document.querySelector('[data-charge-gst="tax_invoice"]')).not.toBeNull());
  });
});
