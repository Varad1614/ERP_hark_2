/**
 * Part 2 · S4 (UI docs): client-side file hints that mirror the server rules, the upload request
 * (raw body, the file's own Content-Type, `?filename=`, never an Idempotency-Key), upload error
 * states in the dialog, and downloads.
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ApiProblem } from "@sutra/api-client";
import { ToastProvider } from "@sutra/ui";
import { describe, expect, it, vi } from "vitest";
import { createHttpRecordApi } from "./api";
import { checkDocumentFile, contentTypeOf, DocumentUploadDialog, filenameOf, uploadMessage, type AttachedFile } from "./files";

const PDF = "%PDF-1.7\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n";
const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13]);
const pdf = (name = "Aadhaar card.pdf", type = "application/pdf") => new File([PDF], name, { type });

const problem = (status: number, code?: string, detail?: string) =>
  new ApiProblem({ type: "about:blank", title: "x", status, detail, errorCode: code ? "validation-failed" : `http-${status}`, validationErrors: code ? [{ field: "file", code, message: code }] : undefined });

describe("client-side file hints (the server still enforces)", () => {
  it("accepts PDF / PNG / JPEG whose content matches the name", async () => {
    expect(await checkDocumentFile(pdf())).toBeNull();
    expect(await checkDocumentFile(new File([PNG], "photo.png", { type: "image/png" }))).toBeNull();
    expect(await checkDocumentFile(new File([new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 1, 2])], "scan.JPEG", { type: "image/jpeg" }))).toBeNull();
    expect(contentTypeOf("HSC marks.PDF")).toBe("application/pdf");
    expect(contentTypeOf("scan.jpg")).toBe("image/jpeg");
    expect(contentTypeOf("notes.docx")).toBeNull();
  });

  it("refuses empty, too large, other types and renamed files", async () => {
    expect(await checkDocumentFile(new File([], "a.pdf", { type: "application/pdf" }))).toMatch(/empty/);
    expect(await checkDocumentFile(pdf(), 10)).toMatch(/limit is/);
    expect(await checkDocumentFile(new File(["x"], "notes.docx"))).toMatch(/Only PDF, PNG or JPEG/);
    expect(await checkDocumentFile(new File(["x"], "noext"))).toMatch(/Only PDF, PNG or JPEG/);
    // A PNG renamed to .pdf: the bytes say otherwise.
    expect(await checkDocumentFile(new File([PNG], "fake.pdf"))).toMatch(/not a PDF/);
    expect(await checkDocumentFile(pdf("a.pdf", "image/png"))).toMatch(/says it is image\/png/);
  });

  it("puts the server's refusals in plain words", () => {
    expect(uploadMessage(problem(422, "active_content"))).toMatch(/scripts, launch actions or embedded files/);
    expect(uploadMessage(problem(422, "extension_mismatch"))).toMatch(/extension does not match/);
    expect(uploadMessage(problem(413))).toMatch(/larger than the upload limit/);
    expect(uploadMessage(problem(409, undefined, "a file is already attached; hand in a new document to replace it"))).toMatch(/already attached/);
    expect(uploadMessage(problem(503))).toMatch(/storage is not available/);
    expect(filenameOf('attachment; filename="HSC marks.pdf"')).toBe("HSC marks.pdf");
    expect(filenameOf(null, "document")).toBe("document");
  });
});

describe("HTTP upload / download", () => {
  const attached: AttachedFile = {
    documentId: "d1",
    documentVersion: 2,
    file: { id: "o1", ownerType: "admission_document", ownerId: "d1", filename: "HSC marks.pdf", contentType: "application/pdf", sizeBytes: 3, sha256: "ab", uploadedAt: "2026-07-01T00:00:00Z", retentionUntil: "2034-07-01T00:00:00Z", legalHold: false, version: 1 },
  };

  it("PUTs the raw file with its own content type, ?filename= and no Idempotency-Key (also above 1 MiB)", async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    const fetchImpl = vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify(attached), { status: 201, headers: { "content-type": "application/json" } });
    });
    const api = createHttpRecordApi(fetchImpl, "");
    const big = new File([PDF, "%".repeat(1536 * 1024)], "HSC marks.pdf", { type: "" });
    await api.uploadAdmissionFile("a1", "d1", big);
    await api.uploadDocumentFile("s1", "d2", new File([PNG], "photo.png", { type: "image/png" }));
    expect(calls[0]!.url).toBe("/api/v1/student/admissions/a1/documents/d1/file?filename=HSC%20marks.pdf");
    expect(calls[1]!.url).toBe("/api/v1/student/students/s1/documents/d2/file?filename=photo.png");
    for (const [i, type] of [[0, "application/pdf"], [1, "image/png"]] as const) {
      const h = calls[i]!.init.headers as Record<string, string>;
      expect(calls[i]!.init.method).toBe("PUT");
      expect(h["Content-Type"]).toBe(type);
      expect(Object.keys(h).map((k) => k.toLowerCase())).not.toContain("idempotency-key");
      expect(calls[i]!.init.body).toBeInstanceOf(File);
    }
    expect((calls[0]!.init.body as File).size).toBeGreaterThan(1024 * 1024);
  });

  it("maps a refused upload to ApiProblem and downloads attachments with their name", async () => {
    const fetchImpl = vi.fn(async (url: string, _init?: RequestInit) =>
      url.endsWith("/file?filename=x.pdf")
        ? new Response(JSON.stringify({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "file", code: "active_content", message: "x" }] }), { status: 422 })
        : new Response(new Blob([PDF], { type: "application/pdf" }), { status: 200, headers: { "content-type": "application/pdf", "content-disposition": 'attachment; filename="Aadhaar card.pdf"', "x-document-sha256": "abc" } }),
    );
    const api = createHttpRecordApi(fetchImpl, "");
    const err = await api.uploadAdmissionFile("a1", "d1", pdf("x.pdf")).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiProblem);
    expect(uploadMessage(err)).toMatch(/embedded files/);
    const f = await api.downloadMyDocumentFile("d9");
    expect(fetchImpl).toHaveBeenLastCalledWith("/api/v1/student/me/documents/d9/file", expect.objectContaining({ method: "GET", credentials: "include" }));
    expect([f.filename, f.contentType, f.sha256]).toEqual(["Aadhaar card.pdf", "application/pdf", "abc"]);
    expect(f.blob.size).toBe(PDF.length);
  });
});

describe("DocumentUploadDialog error states", () => {
  const renderDialog = (onUpload: (f: File) => Promise<AttachedFile>, onDone = vi.fn()) => {
    render(
      <ToastProvider>
        <DocumentUploadDialog title="Upload identity proof" eyebrow="Admissions" onUpload={onUpload} onClose={() => {}} onDone={onDone} />
      </ToastProvider>,
    );
    const input = document.querySelector('input[type="file"]') as HTMLInputElement;
    const pick = (f: File) => fireEvent.change(input, { target: { files: [f] } });
    const submit = () => screen.getByRole("button", { name: /^Upload/ });
    return { pick, submit, onDone };
  };

  it("blocks a wrong type before any request", async () => {
    const onUpload = vi.fn();
    const { pick, submit } = renderDialog(onUpload);
    pick(new File(["x"], "notes.docx"));
    expect((await screen.findByRole("alert")).textContent).toMatch(/Only PDF, PNG or JPEG/);
    expect((submit() as HTMLButtonElement).disabled).toBe(true);
    expect(onUpload).not.toHaveBeenCalled();
  });

  it("shows the server's refusal and lets the user pick another file", async () => {
    const onUpload = vi.fn(async () => {
      throw problem(422, "active_content");
    });
    const { pick, submit, onDone } = renderDialog(onUpload);
    pick(pdf());
    await waitFor(() => expect((submit() as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(submit());
    expect((await screen.findByRole("alert")).textContent).toMatch(/embedded files/);
    expect(onDone).not.toHaveBeenCalled();
    pick(pdf("clean.pdf"));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  it("shows a storage outage and a conflict, then succeeds", async () => {
    let n = 0;
    const onUpload = vi.fn(async () => {
      n++;
      if (n === 1) throw problem(503);
      if (n === 2) throw problem(409, undefined, "a file is already attached; hand in a new document to replace it");
      return { documentId: "d1", documentVersion: 2 } as AttachedFile;
    });
    const { pick, submit, onDone } = renderDialog(onUpload);
    pick(pdf());
    await waitFor(() => expect((submit() as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(submit());
    expect((await screen.findByRole("alert")).textContent).toMatch(/storage is not available/);
    pick(pdf());
    await waitFor(() => expect((submit() as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(submit());
    expect((await screen.findByRole("alert")).textContent).toMatch(/already attached/);
    pick(pdf());
    await waitFor(() => expect((submit() as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(submit());
    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
  });
});

describe("mock record API keeps the server's file rules", () => {
  it("one file per submitted document, replacement by a new document, student downloads the verified file", async () => {
    const { createMockRecordApi } = await import("./mock");
    const api = createMockRecordApi();
    const inst = "01930000-0000-7000-8000-0000000000aa";
    let app = await api.createApplication(inst, { programmeId: "01930000-0000-7000-8000-00000000beef", givenName: "Aarav", familyName: "Desai", subjectRef: "dev|aarav.desai@sutra.dev" });
    app = await api.applicationCommand(app.id, app.version, "submit");
    await api.offer(app.id, app.version, { requiredDocuments: ["identity_proof"] });
    let adm = (await api.listAdmissions(inst))[0]!;
    adm = await api.admissionCommand(adm.id, adm.version, "accept");
    const d = await api.addAdmissionDocument(adm.id, { documentType: "identity_proof", title: "Aadhaar" });
    await expect(api.uploadAdmissionFile(adm.id, d.id, new File(["x"], "a.docx"))).rejects.toBeInstanceOf(ApiProblem);
    const r = await api.uploadAdmissionFile(adm.id, d.id, pdf());
    expect(r.documentVersion).toBe(2);
    await expect(api.uploadAdmissionFile(adm.id, d.id, pdf())).rejects.toMatchObject({ problem: { status: 409 } });
    const back = await api.downloadAdmissionFile(adm.id, d.id);
    expect([back.filename, back.contentType, back.blob.size]).toEqual(["Aadhaar card.pdf", "application/pdf", PDF.length]);
  });
});
