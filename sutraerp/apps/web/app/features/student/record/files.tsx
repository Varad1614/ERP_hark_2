/**
 * Part 2 · S4 (UI docs): the stored files of admission documents and of documents on the
 * student's record (S4-docs endpoints, platform document service on S3-compatible storage).
 *
 * - `checkDocumentFile` — client-side hints that mirror the server rules
 *   (`modules/platform/src/documents/validate.rs`): PDF / PNG / JPEG only, the extension must
 *   match the content (magic bytes), not empty, at most `DOCUMENT_MAX_BYTES` (the default
 *   `SUTRA_DOCS_MAX_BYTES`; nginx allows 11 MiB on the upload routes). **The server still
 *   enforces every rule** (and refuses PDFs with active content, which the browser cannot see).
 * - `uploadMessage` — the server's refusals (422 codes, 409, 413, 503) in plain words.
 * - `DocumentUploadDialog` — FileField (prototype `.x-file`) + hint + inline error, uploads with
 *   the file's own `Content-Type` and `?filename=`, never an `Idempotency-Key` (a document takes
 *   one file; a retried upload is a 409 and the first file stands).
 * - `saveDownloadedFile` — streams a downloaded attachment to the browser's save dialog.
 */
import { FileUp } from "lucide-react";
import { FileField, StepDialog, useToast } from "@sutra/ui";
import { ApiProblem, type components } from "@sutra/api-client";
import { useCallback, useState } from "react";

export type AttachedFile = components["schemas"]["AttachedFile"];

/** Default `SUTRA_DOCS_MAX_BYTES` (10 MiB). */
export const DOCUMENT_MAX_BYTES = 10 * 1024 * 1024;
export const DOCUMENT_ACCEPT = ".pdf,.png,.jpg,.jpeg,application/pdf,image/png,image/jpeg";
export const DOCUMENT_HINT = "PDF, PNG or JPEG · up to 10 MB";

const TYPES: Record<string, { mime: string; magic: number[][] }> = {
  pdf: { mime: "application/pdf", magic: [[0x25, 0x50, 0x44, 0x46, 0x2d]] },
  png: { mime: "image/png", magic: [[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]] },
  jpg: { mime: "image/jpeg", magic: [[0xff, 0xd8, 0xff]] },
  jpeg: { mime: "image/jpeg", magic: [[0xff, 0xd8, 0xff]] },
};

export function extensionOf(name: string): string | null {
  const base = name.split(/[\\/]/).pop() ?? "";
  const i = base.lastIndexOf(".");
  return i > 0 ? base.slice(i + 1).toLowerCase() : null;
}

/** The file's own media type, from its (allowed) extension; `null` when the type is not allowed. */
export function contentTypeOf(name: string): string | null {
  const ext = extensionOf(name);
  const t = ext ? TYPES[ext] : undefined;
  return t ? t.mime : null;
}

async function head(file: Blob, n: number): Promise<Uint8Array | null> {
  try {
    const slice = file.slice(0, n);
    if (typeof slice.arrayBuffer === "function") return new Uint8Array(await slice.arrayBuffer());
    return await new Promise<Uint8Array | null>((resolve) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result instanceof ArrayBuffer ? new Uint8Array(r.result) : null);
      r.onerror = () => resolve(null);
      r.readAsArrayBuffer(slice);
    });
  } catch {
    return null;
  }
}

/**
 * Client-side check of a picked file (`null` = looks fine; the server decides). Mirrors the
 * server's order: empty → too large → type not allowed → content does not match the extension.
 */
export async function checkDocumentFile(file: File, max = DOCUMENT_MAX_BYTES): Promise<string | null> {
  if (file.size === 0) return "The file is empty.";
  if (file.size > max) return `The file is ${(file.size / 1048576).toFixed(1)} MB; the limit is ${Math.round(max / 1048576)} MB.`;
  const ext = extensionOf(file.name);
  const t = ext ? TYPES[ext] : undefined;
  if (!t) return "Only PDF, PNG or JPEG files can be uploaded.";
  if (file.type && file.type !== t.mime && !(t.mime === "image/jpeg" && file.type === "image/jpg")) return `The file says it is ${file.type}, but its name ends in .${ext}.`;
  const bytes = await head(file, 8);
  if (bytes && !t.magic.some((m) => m.every((b, i) => bytes[i] === b))) return `The content of this file is not a ${ext === "jpg" || ext === "jpeg" ? "JPEG" : ext!.toUpperCase()} — rename it to its real type or export it again.`;
  return null;
}

const CODES: Record<string, string> = {
  empty: "The file is empty.",
  too_large: "The file is larger than the upload limit.",
  type_not_allowed: "Only PDF, PNG or JPEG files can be uploaded.",
  type_mismatch: "The file's content does not match its type.",
  extension_mismatch: "The file name's extension does not match its content.",
  active_content: "This PDF contains scripts, launch actions or embedded files and was refused. Print it to a plain PDF and upload that.",
  invalid: "The file name is not valid.",
};

/** The server's refusal of an upload in plain words. */
export function uploadMessage(e: unknown): string {
  if (e instanceof ApiProblem) {
    const p = e.problem;
    const v = p.validationErrors?.[0];
    if (v) return CODES[v.code] ?? v.message;
    if (p.status === 413) return "The file is larger than the upload limit.";
    if (p.status === 409) return p.detail ?? "A file is already attached to this document — hand in a new document to replace it.";
    if (p.status === 503) return "Document storage is not available right now. Try again later.";
    if (p.status === 403) return "You are not allowed to upload files here.";
    return p.detail ?? p.title;
  }
  return e instanceof Error ? e.message : "The upload failed.";
}

/** A downloaded attachment. */
export interface DownloadedFile {
  blob: Blob;
  filename: string;
  contentType: string;
  sha256: string | null;
}

/** `attachment; filename="x.pdf"` → `x.pdf`. */
export function filenameOf(disposition: string | null, fallback = "document"): string {
  const m = /filename\*?=(?:UTF-8'')?"?([^";]+)"?/i.exec(disposition ?? "");
  if (!m) return fallback;
  try {
    return decodeURIComponent(m[1]!);
  } catch {
    return m[1]!;
  }
}

/** Hands a downloaded file to the browser (save dialog); the object URL is released right after. */
export function saveDownloadedFile(f: DownloadedFile, doc: Document = document) {
  const url = URL.createObjectURL(f.blob);
  const a = doc.createElement("a");
  a.href = url;
  a.download = f.filename;
  a.rel = "noopener";
  a.style.display = "none";
  doc.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/**
 * Upload dialog of one document: pick → client-side hint → upload (inline error on refusal).
 * `onUpload` performs the PUT; `onDone` runs after a 201.
 */
export function DocumentUploadDialog({ title, eyebrow, replace, onUpload, onClose, onDone }: { title: string; eyebrow: string; replace?: boolean; onUpload: (file: File) => Promise<AttachedFile>; onClose: () => void; onDone: (r: AttachedFile) => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const pick = async (f: File | null) => {
    setFile(f);
    setError(f ? await checkDocumentFile(f) : null);
  };
  const upload = async () => {
    if (!file) return;
    const hint = await checkDocumentFile(file);
    if (hint) {
      setError(hint);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const r = await onUpload(file);
      onDone(r);
    } catch (e) {
      setError(uploadMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <StepDialog
      open
      onClose={onClose}
      eyebrow={eyebrow}
      title={title}
      data-upload-dialog=""
      footer={
        <div className="sb-dialog-foot">
          <button type="button" className="sb-btn" disabled={!file || !!error || busy} data-action="document-upload-submit" onClick={() => void upload()}>
            <FileUp strokeWidth={1.75} aria-hidden="true" />
            {busy ? "Uploading…" : replace ? "Upload replacement" : "Upload"}
          </button>
          <button type="button" className="sb-btn sb-btn--ghost" onClick={onClose}>
            Cancel
          </button>
        </div>
      }
    >
      <div data-form="document-upload">
        {replace && <p style={{ color: "var(--sb-mute)", fontSize: 14, margin: "0 0 12px" }}>The rejected copy stays on file for the record; the replacement is checked again.</p>}
        <FileField label="Choose a file" name="file" accept={DOCUMENT_ACCEPT} hint={DOCUMENT_HINT} fileName={file?.name} onFile={(f) => void pick(f)} />
        {error && (
          <p role="alert" className="sb-field-error" data-upload-error style={{ color: "var(--sb-danger, #B4232C)", fontSize: 13.5, marginTop: 10 }}>
            {error}
          </p>
        )}
      </div>
    </StepDialog>
  );
}

/** Download errors in plain words (409 = integrity check failed, the file is quarantined). */
export function downloadMessage(e: unknown): string {
  if (e instanceof ApiProblem) {
    const p = e.problem;
    if (p.status === 409) return "This file failed its integrity check and is held for review — contact the registrar’s office.";
    if (p.status === 404) return "No file is stored for this document.";
    if (p.status === 503) return "Document storage is not available right now. Try again later.";
    if (p.status === 403) return "You are not allowed to download this file.";
    return p.detail ?? p.title;
  }
  return e instanceof Error ? e.message : "The download failed.";
}

/** Runs a download and hands the file to the browser; errors become a toast. */
export function useDownload() {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const download = useCallback(
    async (fetchFile: () => Promise<DownloadedFile>) => {
      setBusy(true);
      try {
        saveDownloadedFile(await fetchFile());
      } catch (e) {
        toast(downloadMessage(e));
      } finally {
        setBusy(false);
      }
    },
    [toast],
  );
  return { download, busy };
}
