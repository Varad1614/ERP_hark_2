/** Slice 8D web: mock API behaviour, HTTP request shape, action rules and the three Campus tabs. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createHttpResourcesApi, PATHS } from "./api";
import { createMockResourcesApi, fromMilli, milli } from "./mock";
import { assetActions, invoiceActions, orderActions, requisitionActions, supplierActions, toInvoice } from "./index";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("8D resources API (mock)", () => {
  it("runs requisition → RFQ → award → order → receipt → invoice → payment", async () => {
    const api = createMockResourcesApi();
    const [req] = await api.requisitions("inst-1");
    const [sup] = await api.suppliers("inst-1");
    expect(req?.status).toBe("released");
    let ev = await api.createSourcingEvent("inst-1", { requisitionId: req?.id ?? "", method: "rfq" });
    ev = await api.quote(ev.id, ev.version, { supplierId: sup?.id ?? "", lines: [{ lineNo: 1, unitPrice: "240.00" }] });
    expect(ev.quotations?.[0]?.totalAmount).toBe("4800.00");
    ev = await api.evaluate(ev.id, ev.version);
    ev = await api.award(ev.id, ev.version, ev.quotations?.[0]?.id ?? "");
    expect(ev.status).toBe("awarded");
    let po = await api.createPurchaseOrder("inst-1", { sourcingEventId: ev.id });
    expect(po.totalAmount).toBe("4800.00");
    po = await api.issueOrder(po.id, po.version);
    await expect(api.receive(po.id, { lines: [{ lineNo: 1, quantity: "25" }] })).rejects.toThrow();
    const g = await api.receive(po.id, { lines: [{ lineNo: 1, quantity: "20" }] });
    await api.acceptReceipt(g.id, g.version);
    const stock = await api.stock("inst-1");
    expect(stock[0]?.onHand).toBe("60.000");
    po = await api.purchaseOrder(po.id);
    expect(po.status).toBe("received");
    const bad = await api.createInvoice("inst-1", { purchaseOrderId: po.id, supplierInvoiceNumber: "X-1", invoiceDate: "2026-11-20", lines: [{ lineNo: 1, quantity: "20", unitPrice: "300.00" }] });
    const mism = await api.matchInvoice(bad.id, bad.version);
    expect(mism.status).toBe("mismatch");
    await expect(api.postInvoice(mism.id, mism.version)).rejects.toThrow();
    let inv = await api.createInvoice("inst-1", { purchaseOrderId: po.id, supplierInvoiceNumber: "X-2", invoiceDate: "2026-11-20", lines: [{ lineNo: 1, quantity: "20", unitPrice: "240.00" }] });
    inv = await api.matchInvoice(inv.id, inv.version);
    inv = await api.postInvoice(inv.id, inv.version);
    expect(inv.status).toBe("posted");
    const pay = await api.createPayment("inst-1", { supplierId: inv.supplierId, paymentDate: "2026-11-30", allocations: [{ vendorInvoiceId: inv.id, amount: "4800.00" }] });
    await api.postPayment(pay.id, pay.version);
    expect((await api.invoices("inst-1")).find((x) => x.id === inv.id)?.status).toBe("paid");
  });

  it("refuses issues beyond available stock and keeps reservations", async () => {
    const api = createMockResourcesApi();
    const [item] = await api.items("inst-1");
    const [store] = await api.stores("inst-1");
    await api.reserve("inst-1", { itemId: item?.id ?? "", locationId: store?.id ?? "", quantity: "30", purpose: "Exams" });
    let s = await api.createIssue("inst-1", { itemId: item?.id ?? "", locationId: store?.id ?? "", quantity: "15", purpose: "Office" });
    s = await api.decideIssue(s.id, s.version, "approve");
    await expect(api.issueStock(s.id, s.version)).rejects.toThrow();
  });

  it("does decimal arithmetic without floats", () => {
    expect(milli("12.5")).toBe(12500);
    expect(fromMilli(milli("-0.25"), 2)).toBe("-0.25");
    expect(toInvoice("25.000", "9.000")).toBe("16.000");
    expect(toInvoice("1.000", "2.000")).toBe("0.000");
  });
});

describe("8D HTTP API", () => {
  it("sends If-Match on commands and Idempotency-Key on creates", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "x1", status: "issued", version: 3 }), { status: 200 });
    };
    const api = createHttpResourcesApi(fetchImpl, "");
    await api.issueOrder("po-1", 2);
    await api.createRequisition("inst-1", { title: "Paper", lines: [{ description: "Paper", quantity: "1", estimatedUnitCost: "10.00" }] });
    const h0 = calls[0]?.init.headers as Record<string, string>;
    const h1 = calls[1]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.orderIssue.replace("{id}", "po-1"));
    expect(h0["If-Match"]).toBe('"2"');
    expect(h1["Idempotency-Key"]).toBeTruthy();
    expect(calls[1]?.url).toBe(PATHS.requisitions.replace("{id}", "inst-1"));
  });
});

describe("8D action rules", () => {
  it("offers the next step of each record", () => {
    expect(supplierActions({ status: "prospective", approvalPending: false })).toEqual(["register"]);
    expect(supplierActions({ status: "registered", approvalPending: true })).toEqual(["approve", "reject"]);
    expect(requisitionActions({ status: "released" })).toEqual(["source", "order", "cancel"]);
    expect(orderActions({ status: "received", approvalPending: false })).toEqual(["invoice", "close"]);
    expect(invoiceActions({ status: "mismatch", approvalPending: false, isCreditNote: false })).toEqual(["match"]);
    expect(invoiceActions({ status: "posted", approvalPending: false, isCreditNote: true })).toEqual([]);
    expect(assetActions({ status: "assigned", retirementPending: false })).toEqual(["return", "condition"]);
  });
});

describe("8D Campus tabs", () => {
  it("renders procurement, inventory and assets with the 8D marker", async () => {
    const { renderScreen } = await import("../../../test/harness");
    const { ProcurementAdminTab, InventoryAdminTab, AssetsAdminTab } = await import("./index");
    const institution = { id: "inst-1", name: "Sutra College" } as Parameters<typeof ProcurementAdminTab>[0]["institution"];
    await renderScreen(<ProcurementAdminTab institution={institution} />, { path: "/campus/:tab?", entry: "/campus/procurement" });
    expect(await screen.findByText("SUP-000001")).toBeTruthy();
    expect(document.querySelector("[data-slice='8D']")).toBeTruthy();
    await renderScreen(<InventoryAdminTab institution={institution} />, { path: "/campus/:tab?", entry: "/campus/inventory" });
    expect((await screen.findAllByText("PAPER-A4 · A4 paper (ream)")).length).toBeGreaterThan(0);
    await renderScreen(<AssetsAdminTab institution={institution} />, { path: "/campus/:tab?", entry: "/campus/assets" });
    expect(await screen.findByText("AST-000001")).toBeTruthy();
  });
});
