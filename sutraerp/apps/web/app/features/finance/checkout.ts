/**
 * Part 2 · S4 (UI fees, DEF-F-007): the browser side of online fee payment.
 *
 * - `payStage` — what the Pay dialog shows. **Paid only when the server says `completed`**: the
 *   Razorpay Checkout callback (or any other client-side signal) moves the dialog to
 *   "processing", never to "paid" (the server verifies the signature and asks Razorpay for the
 *   capture before it completes the payment — `POST /me/payments/{id}/confirm`).
 * - `loadRazorpay` — injects `https://checkout.razorpay.com/v1/checkout.js` once, and only when
 *   the configured gateway is `razorpay` (the CSP allows exactly that origin for scripts and
 *   `api.razorpay.com` / `checkout.razorpay.com` for its frames and calls).
 * - `openRazorpayCheckout` — opens Checkout for the server-created order with the public key id
 *   and resolves with the handler response (forwarded to `confirm`), a dismissal or the last
 *   failure Checkout reported.
 *
 * Razorpay Checkout options / handler response / `payment.failed` event:
 * https://razorpay.com/docs/payments/payment-gateway/web-integration/standard/build-integration/
 */
import type { CheckoutCallback, Payment, PaymentGatewayInfo } from "./payments/types";
import { toPaise } from "./payments/mock";

export const RAZORPAY_CHECKOUT_SRC = "https://checkout.razorpay.com/v1/checkout.js";

export type PayStage = "form" | "checkout" | "processing" | "paid" | "failed";

const CLOSED = new Set(["failed", "expired", "cancelled"]);

/**
 * Stage of the Pay dialog. `verifying` = a confirm call is in flight; `callbackReceived` = the
 * gateway's checkout reported success in the browser (unverified until the server confirms).
 */
export function payStage(payment: Pick<Payment, "status"> | null, opts: { verifying?: boolean; callbackReceived?: boolean } = {}): PayStage {
  if (!payment) return "form";
  if (payment.status === "completed") return "paid";
  if (CLOSED.has(payment.status)) return "failed";
  if (opts.verifying || opts.callbackReceived) return "processing";
  return "checkout";
}

/** Short label of the configured gateway for the checkout line. */
export function gatewayLabel(gateway: string | null | undefined): string {
  if (gateway === "razorpay") return "Razorpay";
  if (gateway === "mock") return "test gateway — no real money moves";
  return "online payment";
}

/** `true` when the student can pay online (an adapter other than `disabled` is configured). */
export function canPayOnline(info: PaymentGatewayInfo | undefined): boolean {
  return !!info && info.onlinePayments && (info.gateway !== "razorpay" || !!info.checkoutKeyId);
}

// ---------------------------------------------------------------- Razorpay Checkout

export interface RazorpayFailure {
  code?: string;
  description?: string;
  reason?: string;
}

interface RazorpayOptions {
  key: string;
  order_id: string;
  amount: number;
  currency: "INR";
  name: string;
  description: string;
  prefill?: { name?: string };
  notes?: Record<string, string>;
  handler: (response: CheckoutCallback) => void;
  modal: { ondismiss: () => void; confirm_close?: boolean; escape?: boolean };
  retry?: { enabled: boolean };
}

export interface RazorpayInstance {
  open(): void;
  on(event: "payment.failed", cb: (response: { error?: RazorpayFailure }) => void): void;
}

export type RazorpayCtor = new (options: RazorpayOptions) => RazorpayInstance;

type RazorpayWindow = Window & { Razorpay?: RazorpayCtor };

let loading: Promise<RazorpayCtor> | null = null;

/** Loads Razorpay Checkout once (script from checkout.razorpay.com only). */
export function loadRazorpay(doc: Document = document): Promise<RazorpayCtor> {
  const w = (doc.defaultView ?? window) as RazorpayWindow;
  if (w.Razorpay) return Promise.resolve(w.Razorpay);
  loading ??= new Promise<RazorpayCtor>((resolve, reject) => {
    const s = doc.createElement("script");
    s.src = RAZORPAY_CHECKOUT_SRC;
    s.async = true;
    s.dataset.razorpayCheckout = "";
    s.onload = () => (w.Razorpay ? resolve(w.Razorpay) : reject(new Error("Razorpay Checkout did not load")));
    s.onerror = () => {
      loading = null;
      s.remove();
      reject(new Error("Razorpay Checkout could not be loaded — check your connection and try again"));
    };
    doc.head.appendChild(s);
  });
  return loading;
}

/** Test hook: forget a previous (failed) load. */
export function resetRazorpayLoader() {
  loading = null;
}

export type CheckoutResult = { kind: "callback"; callback: CheckoutCallback } | { kind: "dismissed" } | { kind: "failed"; failure: RazorpayFailure };

/**
 * Opens Razorpay Checkout for the server-created order of `payment`. Resolves once: with the
 * handler response (to be verified by the server), or — when the payer closes Checkout — with the
 * last failure Checkout reported (`payment.failed`; Checkout lets the payer retry the same order
 * before that) or a plain dismissal.
 */
export function openRazorpayCheckout(Ctor: RazorpayCtor, keyId: string, payment: Pick<Payment, "amount" | "gatewayOrderRef" | "paymentNumber">, opts: { name: string; description: string; payer?: string }): Promise<CheckoutResult> {
  return new Promise((resolve) => {
    let done = false;
    let last: RazorpayFailure | null = null;
    const finish = (r: CheckoutResult) => {
      if (done) return;
      done = true;
      resolve(r);
    };
    const rz = new Ctor({
      key: keyId,
      order_id: payment.gatewayOrderRef ?? "",
      amount: Number(toPaise(payment.amount)),
      currency: "INR",
      name: opts.name,
      description: opts.description,
      prefill: opts.payer ? { name: opts.payer } : undefined,
      notes: { payment: payment.paymentNumber },
      handler: (response) => finish({ kind: "callback", callback: { razorpay_payment_id: response.razorpay_payment_id, razorpay_order_id: response.razorpay_order_id, razorpay_signature: response.razorpay_signature } }),
      modal: { ondismiss: () => finish(last ? { kind: "failed", failure: last } : { kind: "dismissed" }), confirm_close: true, escape: true },
      retry: { enabled: true },
    });
    rz.on("payment.failed", (r) => {
      last = r?.error ?? { description: "payment failed" };
    });
    rz.open();
  });
}
