/**
 * Part 2 · S4 (UI fees, DEF-F-007): the Pay dialog shows "paid" ONLY after the server confirms
 * (`payment.status === "completed"` from `POST /me/payments/{id}/confirm`); a Razorpay Checkout
 * callback alone moves it to "processing". Failed / pending states offer a retry; a disabled
 * gateway hides "Pay now".
 */
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { canPayOnline, gatewayLabel, payStage } from "./checkout";
import type { ChargeDetail } from "./charges/types";
import type { CheckoutCallback, Payment, PaymentGatewayInfo } from "./payments/types";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});
afterEach(() => {
  delete (window as { Razorpay?: unknown }).Razorpay;
});

const CHARGE = {
  id: "c1",
  chargeNumber: "CHG-000001",
  description: "Tuition fee — term 1",
  totalAmount: "125000.00",
  outstandingAmount: "125000.00",
  settledAmount: "0.00",
  adjustmentAmount: "0.00",
  dueDate: "2026-07-15",
  status: "published",
  instalments: [],
  lines: [],
} as unknown as ChargeDetail;

const RAZORPAY: PaymentGatewayInfo = { gateway: "razorpay", onlinePayments: true, checkoutKeyId: "rzp_test_Ui0000000001" };
const MOCK: PaymentGatewayInfo = { gateway: "mock", onlinePayments: true, checkoutKeyId: null };

function payment(status: Payment["status"], extra: Partial<Payment> = {}): Payment {
  return {
    id: "p1",
    paymentNumber: "PAY-000001",
    institutionId: "i1",
    studentAccountId: "a1",
    channel: "gateway",
    payerKind: "student",
    amount: "125000.00",
    currency: "INR",
    status,
    gateway: "razorpay",
    gatewayOrderRef: "order_Ui0000000001",
    receivedOn: "2026-07-01",
    allocations: [],
    unappliedAmount: "0.00",
    createdAt: "2026-07-01T10:00:00Z",
    version: 1,
    ...extra,
  };
}

/** Razorpay Checkout stand-in: `open()` answers with the handler (a successful payment in the browser). */
function fakeCheckout(onOpen: (o: { handler: (r: CheckoutCallback) => void; order_id: string; key: string; amount: number; modal: { ondismiss: () => void } }, fail: (e: object) => void) => void) {
  const opened: { key: string; order_id: string; amount: number }[] = [];
  class Razorpay {
    private failed: ((r: { error: object }) => void) | null = null;
    constructor(private readonly o: { handler: (r: CheckoutCallback) => void; order_id: string; key: string; amount: number; modal: { ondismiss: () => void } }) {}
    on(_e: string, cb: (r: { error: object }) => void) {
      this.failed = cb;
    }
    open() {
      opened.push({ key: this.o.key, order_id: this.o.order_id, amount: this.o.amount });
      setTimeout(() => onOpen(this.o, (e) => this.failed?.({ error: e })), 0);
    }
  }
  (window as unknown as { Razorpay: unknown }).Razorpay = Razorpay;
  return opened;
}

async function renderPay(gateway: PaymentGatewayInfo, overrides: Record<string, unknown>) {
  const { renderScreen } = await import("../../test/harness");
  const { PayDialog } = await import("./fees");
  const { PaymentsApiProvider } = await import("./payments/hooks");
  const { createMockPaymentsApi } = await import("./payments/mock");
  const api = { ...createMockPaymentsApi(), ...overrides };
  const onClose = vi.fn();
  await renderScreen(
    <PaymentsApiProvider api={api as never}>
      <PayDialog charge={CHARGE} gateway={gateway} payer="Aarav Desai" onClose={onClose} onReceipt={() => {}} />
    </PaymentsApiProvider>,
    { role: "student", path: "/fees" },
  );
  return { api, onClose };
}

const stage = () => document.querySelector("[data-pay-stage]")?.getAttribute("data-pay-stage");

describe("payStage — paid only on the server's word", () => {
  it("never reports paid for a payment the server has not completed", () => {
    expect(payStage(null)).toBe("form");
    expect(payStage(payment("pending"))).toBe("checkout");
    expect(payStage(payment("pending"), { callbackReceived: true })).toBe("processing");
    expect(payStage(payment("initiated"), { verifying: true })).toBe("processing");
    for (const s of ["failed", "expired", "cancelled"] as const) expect(payStage(payment(s), { callbackReceived: true })).toBe("failed");
    expect(payStage(payment("completed"))).toBe("paid");
  });

  it("labels the gateway and hides online payment when it is off", () => {
    expect(gatewayLabel("razorpay")).toBe("Razorpay");
    expect(gatewayLabel("mock")).toContain("test gateway");
    expect(canPayOnline(RAZORPAY)).toBe(true);
    expect(canPayOnline(MOCK)).toBe(true);
    expect(canPayOnline({ gateway: "disabled", onlinePayments: false, checkoutKeyId: null })).toBe(false);
    expect(canPayOnline({ gateway: "razorpay", onlinePayments: true, checkoutKeyId: null })).toBe(false);
    expect(canPayOnline(undefined)).toBe(false);
  });
});

describe("Pay dialog with Razorpay Checkout", () => {
  it("shows processing after the checkout callback and paid only after the server confirms", async () => {
    const opened = fakeCheckout((o) => o.handler({ razorpay_payment_id: "pay_Ui0000000001", razorpay_order_id: o.order_id, razorpay_signature: "f".repeat(64) }));
    const confirms: { callback?: CheckoutCallback }[] = [];
    let serverSays: Payment["status"] = "pending";
    await renderPay(RAZORPAY, {
      initiate: async () => payment("pending"),
      confirm: async (_id: string, _v: number, callback?: CheckoutCallback) => {
        confirms.push({ callback });
        return serverSays === "completed" ? payment("completed", { version: 3, receipt: { id: "r1", receiptNumber: "RCT-000009", status: "issued", issuedAt: "2026-07-01T10:01:00Z" } }) : payment(serverSays, { version: 2 });
      },
    });
    fireEvent.click(await screen.findByRole("button", { name: /Pay ₹1,25,000/ }));
    // Checkout opened with the public key and the server's order (amount in paise).
    await waitFor(() => expect(opened).toEqual([{ key: "rzp_test_Ui0000000001", order_id: "order_Ui0000000001", amount: 12500000 }]));
    // The callback went to the server for verification; the server still says pending → processing, NOT paid.
    await waitFor(() => expect(confirms).toHaveLength(1));
    expect(confirms[0]!.callback).toEqual({ razorpay_payment_id: "pay_Ui0000000001", razorpay_order_id: "order_Ui0000000001", razorpay_signature: "f".repeat(64) });
    await waitFor(() => expect(stage()).toBe("processing"));
    expect(document.querySelector("[data-paid]")).toBeNull();
    expect(screen.queryByText(/paid$/)).toBeNull();
    expect(await screen.findByText(/Payment processing/, { selector: "b" })).toBeTruthy();
    // The server confirms the capture → paid with the receipt number.
    serverSays = "completed";
    fireEvent.click(screen.getByRole("button", { name: /Check again/ }));
    await waitFor(() => expect(stage()).toBe("paid"));
    expect(document.querySelector("[data-receipt-number]")?.textContent).toBe("RCT-000009");
    expect(confirms[1]!.callback).toBeUndefined();
  });

  it("stays unpaid when the server refuses a forged callback", async () => {
    fakeCheckout((o) => o.handler({ razorpay_payment_id: "pay_Forged", razorpay_order_id: o.order_id, razorpay_signature: "0".repeat(64) }));
    const { ApiProblem } = await import("@sutra/api-client");
    await renderPay(RAZORPAY, {
      initiate: async () => payment("pending"),
      confirm: async () => {
        throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "gatewaySignature", code: "invalid_signature", message: "the checkout signature does not verify" }] });
      },
    });
    fireEvent.click(await screen.findByRole("button", { name: /Pay ₹1,25,000/ }));
    await waitFor(() => expect(document.querySelector("[data-pay-notice]")?.textContent).toContain("could not confirm"));
    expect(stage()).toBe("processing");
    expect(document.querySelector("[data-paid]")).toBeNull();
  });

  it("offers a retry when Checkout reports a failure and the payer closes it", async () => {
    let attempt = 0;
    const opened = fakeCheckout((o, fail) => {
      attempt++;
      if (attempt === 1) {
        fail({ code: "BAD_REQUEST_ERROR", description: "Card declined" });
        o.modal.ondismiss();
      } else o.handler({ razorpay_payment_id: "pay_Ok", razorpay_order_id: o.order_id, razorpay_signature: "a".repeat(64) });
    });
    await renderPay(RAZORPAY, {
      initiate: async () => payment("pending"),
      confirm: async () => payment("completed", { version: 2, receipt: { id: "r2", receiptNumber: "RCT-000010", status: "issued", issuedAt: "2026-07-01T10:01:00Z" } }),
    });
    fireEvent.click(await screen.findByRole("button", { name: /Pay ₹1,25,000/ }));
    await waitFor(() => expect(document.querySelector("[data-pay-notice]")?.textContent).toContain("Card declined"));
    expect(stage()).toBe("checkout");
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(stage()).toBe("paid"));
    expect(opened).toHaveLength(2);
    expect(opened[1]!.order_id).toBe("order_Ui0000000001");
  });

  it("starts a new payment after the server marks one failed", async () => {
    fakeCheckout((o) => o.handler({ razorpay_payment_id: "pay_F", razorpay_order_id: o.order_id, razorpay_signature: "b".repeat(64) }));
    const initiate = vi.fn(async () => payment("pending"));
    await renderPay(RAZORPAY, { initiate, confirm: async () => payment("failed", { version: 2 }) });
    fireEvent.click(await screen.findByRole("button", { name: /Pay ₹1,25,000/ }));
    await waitFor(() => expect(stage()).toBe("failed"));
    expect(document.querySelector("[data-paid]")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: /Pay ₹1,25,000/ })).toBeTruthy();
    expect(initiate).toHaveBeenCalledTimes(1);
  });
});

describe("Pay dialog with the mock gateway (dev / CI)", () => {
  it("completes through the server and never loads Razorpay", async () => {
    const confirm = vi.fn(async () => payment("completed", { gateway: "mock", version: 2, receipt: { id: "r3", receiptNumber: "RCT-000011", status: "issued", issuedAt: "2026-07-01T10:01:00Z" } }));
    await renderPay(MOCK, { initiate: async () => payment("pending", { gateway: "mock" }), confirm });
    fireEvent.click(await screen.findByRole("button", { name: /Pay ₹1,25,000/ }));
    expect(await screen.findByText(/test gateway/)).toBeTruthy();
    expect(stage()).toBe("checkout");
    fireEvent.click(screen.getByRole("button", { name: /Complete payment/ }));
    await waitFor(() => expect(stage()).toBe("paid"));
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(document.querySelector("script[data-razorpay-checkout]")).toBeNull();
  });
});

describe("Student Fees with online payment switched off", () => {
  it("hides Pay now and points to the finance office", async () => {
    const { renderScreen } = await import("../../test/harness");
    const { StudentFees } = await import("./fees");
    const { PaymentsApiProvider } = await import("./payments/hooks");
    const { createMockPaymentsApi } = await import("./payments/mock");
    const api = { ...createMockPaymentsApi(), gatewayInfo: async () => ({ gateway: "disabled", onlinePayments: false, checkoutKeyId: null }) };
    await renderScreen(
      <PaymentsApiProvider api={api}>
        <StudentFees />
      </PaymentsApiProvider>,
      { role: "student", path: "/fees" },
    );
    await waitFor(() => expect(document.querySelector('[data-online-payments="off"]')).toBeTruthy());
    expect(document.querySelector("[data-pay]")).toBeNull();
  });
});
