/** Phase B shared form layer: rules mirror the server caps; 422 problems map onto fields. */
import { ApiProblem } from "@sutra/api-client";
import { describe, expect, it } from "vitest";
import { inputAttrs, LIMITS, memberAliases, memberNames, problemFieldErrors, problemMessage, validateField, validateFields } from "./forms";

const today = new Date("2026-10-21T00:00:00Z");
const problem = (extra: Record<string, unknown>) => new ApiProblem({ type: "urn:x", title: "Validation failed", status: 422, errorCode: "validation-failed", ...extra } as never);

describe("validateField", () => {
  it("amount: positive rupees with ≤ 2 decimals, at most ₹1 crore", () => {
    const f = { name: "amount", label: "Amount", rule: "amount" as const, required: true };
    expect(validateField(f, "", today)).toBe("Amount is required");
    expect(validateField(f, "0", today)).toMatch(/more than ₹0/);
    expect(validateField(f, "-5", today)).toMatch(/amount in rupees/);
    expect(validateField(f, "12.345", today)).toMatch(/2 decimals/);
    expect(validateField(f, "10000000.01", today)).toMatch(/at most ₹1,00,00,000/);
    expect(validateField(f, "10000000.00", today)).toBeUndefined();
    expect(validateField(f, "25000.5", today)).toBeUndefined();
  });
  it("seats: whole number 1–10 000", () => {
    const f = { name: "capacity", label: "Seats", rule: "seats" as const };
    expect(validateField(f, "", today)).toBeUndefined();
    expect(validateField(f, "0", today)).toBe("Seats must be from 1 to 10,000");
    expect(validateField(f, "10001", today)).toBe("Seats must be from 1 to 10,000");
    expect(validateField(f, "2.5", today)).toMatch(/whole number/);
    expect(validateField(f, "60", today)).toBeUndefined();
  });
  it("Indian mobile, e-mail and date of birth (age 3–100, not in the future)", () => {
    const m = { name: "phone", label: "Mobile", rule: "mobile" as const };
    for (const ok of ["9876543210", "+91 98200 12345", "098765-43210", "(91) 98765 43210"]) expect(validateField(m, ok, today)).toBeUndefined();
    for (const bad of ["12345", "5876543210", "98765432101"]) expect(validateField(m, bad, today)).toMatch(/Indian mobile/);
    const e = { name: "email", label: "Email", rule: "email" as const };
    expect(validateField(e, "a@b.in", today)).toBeUndefined();
    expect(validateField(e, "a@b", today)).toMatch(/e-mail address/);
    const d = { name: "dateOfBirth", label: "Date of birth", rule: "dob" as const, required: true };
    expect(validateField(d, "2027-01-01", today)).toMatch(/future/);
    expect(validateField(d, "2025-01-01", today)).toMatch(/3–100/);
    expect(validateField(d, "1920-01-01", today)).toMatch(/3–100/);
    expect(validateField(d, "2008-05-04", today)).toBeUndefined();
  });
  it("validateFields returns every problem keyed by field", () => {
    expect(validateFields([{ name: "a", label: "A", required: true }, { name: "s", label: "S", rule: "seats" }], { a: "", s: "0" }, today)).toEqual({ a: "A is required", s: "S must be from 1 to 10,000" });
  });
});

describe("inputAttrs", () => {
  it("numeric inputs carry the server caps", () => {
    expect(inputAttrs({ name: "amount", label: "A", rule: "amount" })).toMatchObject({ type: "number", min: LIMITS.amount.min, max: LIMITS.amount.max, step: "0.01" });
    expect(inputAttrs({ name: "capacity", label: "S", rule: "seats" })).toMatchObject({ type: "number", min: 1, max: 10_000, step: "1" });
  });
});

describe("problem → fields", () => {
  it("normalises pointers, dotted paths and snake_case", () => {
    expect(memberNames("/body/applicant/date_of_birth")).toEqual(["applicant.dateOfBirth", "dateOfBirth"]);
    expect(memberNames("#/amount")).toEqual(["amount"]);
    expect(memberNames("sections[0].capacity")).toEqual(["sections.capacity", "capacity"]);
  });
  it("maps validationErrors[] (field) and errors[] (pointer), aliases, and keeps the rest unmatched", () => {
    const names = ["amount", "sectionCapacity", "fullName"];
    const aliases = memberAliases([{ name: "sectionCapacity", label: "x", member: "capacity" }, { name: "fullName", label: "y", member: ["givenName", "familyName"] }]);
    const a = problemFieldErrors(problem({ validationErrors: [{ field: "amount", code: "amount_too_large", message: "At most ₹1 crore" }, { field: "sections[0].capacity", code: "range", message: "1–10 000 seats" }, { field: "givenName", code: "markup", message: "No markup" }, { field: "channel", code: "x", message: "Pick a channel" }] }), names, aliases);
    expect(a.fields).toEqual({ amount: "At most ₹1 crore", sectionCapacity: "1–10 000 seats", fullName: "No markup" });
    expect(a.unmatched).toEqual(["Pick a channel"]);
    const b = problemFieldErrors(problem({ errors: [{ pointer: "/amount", detail: "Too big" }] }), names);
    expect(b.fields).toEqual({ amount: "Too big" });
  });
});

describe("problemMessage", () => {
  it("prefers the problem detail and never shows 'Request failed (422)'", () => {
    expect(problemMessage(problem({ detail: "Amount must be at most ₹1 crore" }))).toBe("Amount must be at most ₹1 crore");
    const generic = new ApiProblem({ type: "about:blank", title: "Request failed (422)", status: 422, errorCode: "http-422" } as never);
    expect(generic.message).not.toMatch(/Request failed/);
    expect(problemMessage(generic)).toBe("Some details are not valid. Check the highlighted fields.");
    expect(problemMessage(new Error("Request failed (409)"))).toMatch(/changed by someone else/);
    expect(problemMessage(new Error("Network down"))).toBe("Network down");
  });
});
