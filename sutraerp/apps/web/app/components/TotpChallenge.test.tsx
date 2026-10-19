import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { isTotpCode, TotpChallenge } from "./TotpChallenge";

describe("TOTP challenge step", () => {
  it("only accepts six digits", () => {
    expect(isTotpCode("123456")).toBe(true);
    expect(isTotpCode("12345")).toBe(false);
    expect(isTotpCode("12345a")).toBe(false);
    expect(isTotpCode(" 123456")).toBe(false);
  });

  it("submits the code once six digits are entered and strips non-digits", () => {
    const onSubmit = vi.fn();
    render(<TotpChallenge onSubmit={onSubmit} />);
    const input = screen.getByLabelText("Authentication code") as HTMLInputElement;
    const button = screen.getByRole("button", { name: "Verify" });
    expect(input.getAttribute("autocomplete")).toBe("one-time-code");
    fireEvent.change(input, { target: { value: "12 3a4" } });
    expect(input.value).toBe("1234");
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(button);
    expect(onSubmit).not.toHaveBeenCalled();
    fireEvent.change(input, { target: { value: "1234567" } });
    expect(input.value).toBe("123456");
    fireEvent.click(button);
    expect(onSubmit).toHaveBeenCalledWith("123456");
  });

  it("shows the error and offers a way back", () => {
    const onCancel = vi.fn();
    render(<TotpChallenge onSubmit={() => undefined} onCancel={onCancel} error="That code didn’t work." busy={false} />);
    expect(screen.getByText("That code didn’t work.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(onCancel).toHaveBeenCalled();
  });

  it("disables submission while verifying", () => {
    render(<TotpChallenge onSubmit={() => undefined} busy />);
    expect((screen.getByRole("button", { name: "Verifying…" }) as HTMLButtonElement).disabled).toBe(true);
  });
});
