/**
 * Part 2 · S4-ui-import: Admin → Data import — permissions, the maker ≠ checker rule, the HTTP
 * layer (typed client, Idempotency-Key, 413) and the screen (mapping → dry run with row errors,
 * approval hidden from the requester).
 */
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { createSutraClient } from "@sutra/api-client";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Institution } from "../institution/types";
import { can, rolesGrant } from "../../lib/permissions";
import { createHttpImportApi, toBase64, type ImportApi, type ImportJob } from "./api";
import { createMockImportApi } from "./mock";

const INST = { id: "inst-imp", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;
const ADMIN = "dev|admin@sutra.dev";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

const b64 = (s: string) => toBase64(new TextEncoder().encode(s));

async function renderTab(api: ImportApi, tab: "import" | "approvals" | "history" | "export", entry = "/data-import") {
  const { renderScreen } = await import("../../test/harness");
  const { DataImportTab } = await import("./index");
  const { ImportApiProvider } = await import("./hooks");
  return renderScreen(
    <ImportApiProvider api={api}>
      <DataImportTab tab={tab} institution={INST} />
    </ImportApiProvider>,
    { path: "/data-import", entry },
  );
}

function pendingJob(id: string, createdBy: string, commitRequestedBy = createdBy): ImportJob {
  const now = "2026-10-16T09:00:00Z";
  return {
    id,
    entity: "opening_balances",
    institutionId: INST.id,
    jobKey: id,
    fileName: `${id}.csv`,
    fileFormat: "csv",
    fileBytes: 120,
    fileSha256: id,
    headers: ["student_number", "amount"],
    mapping: { student_number: "student_number", amount: "amount" },
    onExisting: "skip",
    batchSize: 500,
    requiresApproval: true,
    status: "pending_approval",
    actions: { create: 2, update: 0, skip: 0, error: 0, pending: 0 },
    summary: { rowsInFile: 2, created: 0, updated: 0, skipped: 0, errors: 0, duplicatesInFile: 0, existingInDatabase: 0, batchesCommitted: 0, fileTotal: "17599.75" },
    createdAt: now,
    updatedAt: now,
    createdBy,
    commitRequestedBy,
    version: 3,
  };
}

describe("data import permissions (client copy of the kernel grants)", () => {
  it("offers import to administrators and approval to administrators and the finance checker", async () => {
    expect(rolesGrant(["administrator"], "platform.import.manage")).toBe(true);
    expect(rolesGrant(["finance_approver"], "platform.import.manage")).toBe(false);
    expect(rolesGrant(["finance_approver"], "platform.import.approve")).toBe(true);
    for (const r of ["faculty", "student", "registrar", "exam_controller", "payroll_approver", "procurement_approver"] as const) {
      expect(rolesGrant([r], "platform.import.manage")).toBe(false);
      expect(rolesGrant([r], "platform.import.approve")).toBe(false);
    }
    expect(can(null, "platform.import.manage")).toBe(false);
    const { importTabsFor } = await import("./index");
    expect(importTabsFor({ role: "administrator", apiRoles: ["administrator"] })).toEqual(["import", "approvals", "history", "export"]);
    expect(importTabsFor({ role: "administrator", apiRoles: ["finance_approver"] })).toEqual(["approvals"]);
    expect(importTabsFor({ role: "faculty", apiRoles: ["faculty"] })).toEqual([]);
  });

  it("never offers the decision to the uploader or the commit requester", async () => {
    const { canDecide, isMaker } = await import("./index");
    const own = pendingJob("j1", ADMIN);
    const other = pendingJob("j2", "dev|registrar.clerk@sutra.dev");
    const requestedByMe = pendingJob("j3", "dev|registrar.clerk@sutra.dev", ADMIN);
    const admin = { role: "administrator" as const, apiRoles: ["administrator" as const], subject: ADMIN };
    expect(isMaker(own, ADMIN)).toBe(true);
    expect(canDecide(own, admin)).toBe(false);
    expect(canDecide(requestedByMe, admin)).toBe(false);
    expect(canDecide(other, admin)).toBe(true);
    expect(canDecide(own, { role: "administrator", apiRoles: ["finance_approver"], subject: "dev|finance.approver@sutra.dev" })).toBe(true);
    expect(canDecide(other, { role: "faculty", apiRoles: ["faculty"], subject: "dev|meera.shah@sutra.dev" })).toBe(false);
    // Unknown principal → never offered.
    expect(canDecide(other, { role: "administrator", apiRoles: ["administrator"] })).toBe(false);
    expect(canDecide({ ...other, status: "validated" }, admin)).toBe(false);
  });

  it("lists required columns that are not mapped", async () => {
    const { missingRequired } = await import("./index");
    const { MOCK_ENTITIES } = await import("./mock");
    expect(missingRequired(MOCK_ENTITIES[0], { "Roll No": "student_number" })).toEqual(["Given name", "Programme code"]);
    expect(missingRequired(MOCK_ENTITIES[0], { a: "student_number", b: "given_name", c: "programme_code" })).toEqual([]);
  });
});

describe("data import HTTP layer (generated client)", () => {
  it("uploads with an Idempotency-Key and CSRF, and reads files as blobs", async () => {
    const seen: Request[] = [];
    const fetchStub = async (req: Request) => {
      seen.push(req);
      if (req.url.includes("/template")) return new Response("student_number\n", { status: 200, headers: { "content-type": "text/csv", "content-disposition": 'attachment; filename="students-template.csv"' } });
      return Response.json(pendingJob("j9", ADMIN), { status: 201 });
    };
    const client = createSutraClient({ baseUrl: "", csrfToken: () => "csrf-imp" });
    const api = createHttpImportApi(client, { fetch: fetchStub, baseUrl: "http://localhost" });
    const job = await api.upload({ entity: "students", institutionId: INST.id, fileName: "s.csv", contentBase64: b64("a\n1\n") });
    expect(job.id).toBe("j9");
    expect(seen[0]?.method).toBe("POST");
    expect(new URL(seen[0]!.url).pathname).toBe("/api/v1/platform/imports");
    expect(seen[0]?.headers.get("Idempotency-Key")).toBeTruthy();
    expect(seen[0]?.headers.get("X-CSRF-Token")).toBe("csrf-imp");
    expect(JSON.parse(await seen[0]!.text()).contentBase64).toBe(b64("a\n1\n"));
    const t = await api.template("students", "xlsx");
    expect(new URL(seen[1]!.url).search).toBe("?format=xlsx");
    expect(t.fileName).toBe("students-template.csv");
    expect(await t.blob.text()).toBe("student_number\n");
    await api.rows("j9", true).catch(() => undefined);
    expect(new URL(seen[2]!.url).searchParams.get("errorsOnly")).toBe("true");
  });

  it("explains a body the proxy / API refuses as too large (413)", async () => {
    const fetchStub = async () => new Response("<html>413 Request Entity Too Large</html>", { status: 413, headers: { "content-type": "text/html" } });
    const api = createHttpImportApi(createSutraClient({ baseUrl: "" }), { fetch: fetchStub, baseUrl: "http://localhost" });
    await expect(api.upload({ entity: "students", institutionId: INST.id, fileName: "big.csv", contentBase64: "QQ==" })).rejects.toThrow(/up to 8 MiB/);
  });

  it("base64-encodes files larger than one chunk", () => {
    const bytes = new Uint8Array(100_000).map((_, i) => i % 251);
    const decoded = Uint8Array.from(atob(toBase64(bytes)), (c) => c.charCodeAt(0));
    expect(decoded).toEqual(bytes);
  });
});

describe("Admin → Data import screen", () => {
  it("maps columns (suggested + saved profile) and shows dry-run row errors with the error report", async () => {
    const api = createMockImportApi();
    const csv = "Roll No,First Name,Programme,Birth\nS2026/0001,Aarav,BTECH-CSE,2005-04-12\nS2026/0002,,BTECH-CSE,12/04/2005\nS2020/0001,Old,BTECH-CSE,\n";
    const job = await api.upload({ entity: "students", institutionId: INST.id, fileName: "legacy.csv", contentBase64: b64(csv) });
    expect(job.mapping).toEqual({});
    await renderTab(api, "import", `/data-import?job=${job.id}`);
    const mapping = await waitFor(() => {
      const el = document.querySelector("[data-import-mapping]");
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    // Nothing suggested from these headers: the required columns are reported.
    expect(within(mapping).getByText(/Required columns not mapped: Student number, Given name, Programme code/)).toBeTruthy();
    // Apply the saved profile, then map the date column by hand.
    fireEvent.change(await within(mapping).findByLabelText("Apply a saved mapping"), { target: { value: "prof-1" } });
    expect((within(mapping).getByLabelText("Imports into for Roll No") as HTMLSelectElement).value).toBe("student_number");
    expect((within(mapping).getByLabelText("Imports into for Programme") as HTMLSelectElement).value).toBe("programme_code");
    fireEvent.change(within(mapping).getByLabelText("Imports into for Birth"), { target: { value: "date_of_birth" } });
    expect(within(mapping).queryByText(/Required columns not mapped/)).toBeNull();
    fireEvent.click(within(mapping).getByRole("button", { name: "Save mapping and dry run" }));
    // Dry run: 1 to create, 1 skipped (already in the database), 1 row with two errors.
    const dry = await waitFor(() => {
      const el = document.querySelector("[data-import-dryrun]");
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    expect(dry.getAttribute("data-import-dryrun")).toBe("invalid");
    expect(within(dry).getByText("Rows with errors").parentElement?.textContent).toContain("1");
    const err = await within(dry).findByText(/Given name is required/);
    expect(err.textContent).toContain("Date of birth must be a date");
    expect(err.closest("tr")?.textContent).toContain("3");
    expect(within(dry).getByRole("button", { name: "Download error report (CSV)" })).toBeTruthy();
    expect((within(dry).getByRole("button", { name: /Commit/ }) as HTMLButtonElement).disabled).toBe(true);
    expect((await api.job(job.id)).mapping).toEqual({ "Roll No": "student_number", "First Name": "given_name", Programme: "programme_code", Birth: "date_of_birth" });
  });

  it("commits a clean file and shows the reconciliation", async () => {
    const api = createMockImportApi();
    const job = await api.upload({ entity: "students", institutionId: INST.id, fileName: "clean.csv", contentBase64: b64("student_number,given_name,programme_code\nS1,Asha,BTECH-CSE\nS2,Ravi,BTECH-CSE\n") });
    await renderTab(api, "import", `/data-import?job=${job.id}`);
    fireEvent.click(await screen.findByRole("button", { name: "Save mapping and dry run" }));
    const commit = await screen.findByRole("button", { name: "Commit 2 rows" });
    await waitFor(() => expect((commit as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(commit);
    const rec = await screen.findByText("Reconciliation");
    const tile = rec.closest(".sb-tile") as HTMLElement;
    expect(tile.getAttribute("data-import-reconciliation")).toBe("committed");
    expect(within(tile).getByText("Created").parentElement?.textContent).toContain("2");
  });

  it("hides Approve / Reject from the requester and offers them to another approver", async () => {
    const api = createMockImportApi({ jobs: [pendingJob("mine", ADMIN), pendingJob("theirs", "dev|accounts.clerk@sutra.dev")] });
    await renderTab(api, "approvals");
    const mine = await waitFor(() => {
      const el = document.querySelector('[data-import-approval="mine"]');
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    expect(mine.querySelector("[data-approve]")).toBeNull();
    expect(mine.querySelector("[data-reject]")).toBeNull();
    expect(mine.textContent).toContain("raised by you");
    const theirs = document.querySelector('[data-import-approval="theirs"]') as HTMLElement;
    expect(theirs.querySelector("[data-approve]")).toBeTruthy();
    // Rejecting needs a reason.
    fireEvent.click(theirs.querySelector("[data-reject]") as HTMLElement);
    const dialog = await screen.findByRole("dialog", { name: "Reject import" });
    const submit = within(dialog).getByRole("button", { name: "Reject" }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    fireEvent.change(within(dialog).getByLabelText("Reason"), { target: { value: "Totals do not match the old ledger" } });
    fireEvent.click(submit);
    await waitFor(async () => expect((await api.job("theirs")).status).toBe("rejected"));
    expect((await api.job("mine")).status).toBe("pending_approval");
  });

  it("shows the requester a waiting note instead of the decision on the job page", async () => {
    const api = createMockImportApi({ jobs: [pendingJob("mine", ADMIN)] });
    await renderTab(api, "import", "/data-import?job=mine");
    const note = await waitFor(() => {
      const el = document.querySelector("[data-import-awaiting]");
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    expect(note.textContent).toContain("another person with import approval decides it");
    expect(document.querySelector("[data-import-pending] [data-approve]")).toBeNull();
  });

  it("refuses a file above the import limit before uploading it", async () => {
    const api = createMockImportApi();
    const upload = vi.spyOn(api, "upload");
    await renderTab(api, "import");
    const input = (await screen.findByLabelText("Choose a CSV or Excel file")) as HTMLInputElement;
    expect(screen.getByText(/up to 8 MiB/)).toBeTruthy();
    const big = new File(["x"], "huge.csv", { type: "text/csv" });
    Object.defineProperty(big, "size", { value: 9 * 1024 * 1024 });
    fireEvent.change(input, { target: { files: [big] } });
    expect((await screen.findByRole("alert")).textContent).toMatch(/huge\.csv is 9 MiB\. Files up to 8 MiB can be imported/);
    expect((screen.getByRole("button", { name: "Upload" }) as HTMLButtonElement).disabled).toBe(true);
    expect(upload).not.toHaveBeenCalled();
  });

  it("lists past jobs in the history", async () => {
    const api = createMockImportApi({ jobs: [pendingJob("old", ADMIN)] });
    await renderTab(api, "history");
    expect(await screen.findByText("old.csv")).toBeTruthy();
    expect(screen.getByText("Waiting for approval")).toBeTruthy();
  });
});
