/**
 * Phase B: the finance dialog form (payment record) uses the shared form layer — amount input
 * bounded to the ₹1 crore cap, on-blur check, 422 `validationErrors[]` mapped to the field and
 * focused, and a human message instead of "Request failed (422)".
 */
import { ApiProblem } from "@sutra/api-client";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { describe, expect, it, vi } from "vitest";
import { useFinanceAsk, type FinAskSpec } from "./ui";

function Harness({ spec }: { spec: FinAskSpec }) {
  const { ask, prompt } = useFinanceAsk();
  useEffect(() => ask(spec), []); // eslint-disable-line react-hooks/exhaustive-deps
  return <>{prompt}</>;
}

const fields: FinAskSpec["fields"] = [
  { name: "channel", label: "Channel", type: "select", required: true, options: [{ value: "cash", label: "Cash" }], initial: "cash" },
  { name: "amount", label: "Amount (₹)", required: true, rule: "amount" },
  { name: "payerName", label: "Payer (sponsor)" },
];

const amountInput = () => document.querySelector<HTMLInputElement>('[data-form="record-payment"] input[name="amount"], form input[name="amount"]')!;

describe("record payment form (shared form layer)", () => {
  it("bounds the amount input and checks it on blur", async () => {
    render(<Harness spec={{ title: "Record a payment", name: "record-payment", submitLabel: "Record", fields, onSubmit: vi.fn() }} />);
    const input = await waitFor(() => amountInput());
    expect(input.type).toBe("number");
    expect(input.max).toBe("10000000");
    expect(input.min).toBe("0.01");
    fireEvent.change(input, { target: { value: "20000000" } });
    fireEvent.blur(input);
    expect(await screen.findByText(/at most ₹1,00,00,000/)).toBeTruthy();
    fireEvent.change(input, { target: { value: "500" } });
    fireEvent.blur(input);
    await waitFor(() => expect(screen.queryByText(/at most ₹1,00,00,000/)).toBeNull());
  });

  it("maps a 422 problem onto the amount field and focuses it", async () => {
    const onSubmit = vi.fn().mockRejectedValue(
      new ApiProblem({ type: "urn:sutraerp:problem:validation-failed", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "/amount", code: "amount_too_large", message: "One payment can be at most ₹1 crore" }] } as never),
    );
    render(<Harness spec={{ title: "Record a payment", name: "record-payment", submitLabel: "Record", fields, onSubmit }} />);
    const input = await waitFor(() => amountInput());
    fireEvent.change(input, { target: { value: "9999999" } });
    await act(async () => {
      fireEvent.submit(input.form!);
    });
    expect(await screen.findByText("One payment can be at most ₹1 crore")).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(amountInput()));
    expect(amountInput().getAttribute("aria-invalid")).toBe("true");
  });

  it("shows the problem's human text, never 'Request failed (422)'", async () => {
    const onSubmit = vi.fn().mockRejectedValue(new ApiProblem({ type: "about:blank", title: "Request failed (422)", status: 422, errorCode: "http-422" } as never));
    render(<Harness spec={{ title: "Record a payment", name: "record-payment", submitLabel: "Record", fields, onSubmit }} />);
    const input = await waitFor(() => amountInput());
    fireEvent.change(input, { target: { value: "100" } });
    await act(async () => {
      fireEvent.submit(input.form!);
    });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).not.toMatch(/Request failed/);
    expect(alert.textContent).toMatch(/not valid/);
  });
});
