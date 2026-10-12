/**
 * Browser side of the dev-mode WebAuthn ceremonies (passkeys / security keys).
 *
 * The API sends `PublicKeyCredentialCreationOptions` / `…RequestOptions` in their JSON form
 * (binary members base64url without padding) wrapped as `{ publicKey: {…} }` — the webauthn-rs
 * wire format. The browser API needs `ArrayBuffer`s and its responses must go back base64url
 * encoded. When the browser has the WebAuthn L3 JSON helpers
 * (`PublicKeyCredential.parseCreationOptionsFromJSON` / `parseRequestOptionsFromJSON` and
 * `credential.toJSON()`, Chrome 129+, Firefox 119+, Safari 18.4+) those are used; otherwise the
 * equivalent conversion below. All verification happens on the server.
 *
 * Ceremonies take an `AbortSignal` so the UI can always get out of "Waiting for your device…"
 * (Cancel button, unmount); aborting also closes the browser / Windows Security prompt.
 */

type Json = Record<string, unknown>;

/**
 * A credential as the generated API types expect it: the OpenAPI schema declares WebAuthn JSON
 * as an opaque `object`, which openapi-typescript renders as `Record<string, never>`.
 */
export type WireCredential = Record<string, never>;

export function base64urlToBuffer(value: string): ArrayBuffer {
  const clean = value.replace(/=+$/, "");
  if (!/^[A-Za-z0-9_-]*$/.test(clean)) throw new TypeError("WebAuthn: value is not base64url");
  const b64 = clean.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(clean.length / 4) * 4, "=");
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out.buffer;
}

export function bufferToBase64url(value: ArrayBuffer | ArrayBufferView): string {
  const bytes = value instanceof ArrayBuffer ? new Uint8Array(value) : new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** True when the browser offers WebAuthn (secure context: https or localhost). */
export function webauthnSupported(): boolean {
  return typeof window !== "undefined" && typeof window.PublicKeyCredential === "function" && !!navigator.credentials;
}

/** Removes `null` members recursively: WebIDL dictionaries reject `null` for non-nullable members. */
function withoutNulls<T>(value: T): T {
  if (Array.isArray(value)) return value.map(withoutNulls) as T;
  if (value && typeof value === "object" && !(value instanceof ArrayBuffer) && !ArrayBuffer.isView(value)) {
    const out: Json = {};
    for (const [k, v] of Object.entries(value as Json)) if (v !== null && v !== undefined) out[k] = withoutNulls(v);
    return out as T;
  }
  return value;
}

/** Accepts both `{ publicKey: {…} }` (webauthn-rs) and the bare options object. */
export function unwrapPublicKey(options: unknown): Json {
  if (!options || typeof options !== "object") throw new TypeError("WebAuthn: the server sent no options");
  const o = options as Json;
  const pk = (o.publicKey && typeof o.publicKey === "object" ? o.publicKey : o) as Json;
  if (typeof pk.challenge !== "string") throw new TypeError("WebAuthn: options have no challenge");
  return withoutNulls(pk);
}

function descriptors(list: unknown): PublicKeyCredentialDescriptor[] | undefined {
  if (!Array.isArray(list)) return undefined;
  return list.map((d: Json) => ({ ...(d as object), id: base64urlToBuffer(String(d.id)) }) as PublicKeyCredentialDescriptor);
}

interface JsonCapable {
  parseCreationOptionsFromJSON?: (o: unknown) => PublicKeyCredentialCreationOptions;
  parseRequestOptionsFromJSON?: (o: unknown) => PublicKeyCredentialRequestOptions;
}
function nativeJson(): JsonCapable | undefined {
  return typeof window !== "undefined" && typeof window.PublicKeyCredential === "function"
    ? (window.PublicKeyCredential as unknown as JsonCapable)
    : undefined;
}

/** Manual JSON → `PublicKeyCredentialCreationOptions` (browsers without the L3 helper). */
export function decodeCreationOptions(pk: Json): PublicKeyCredentialCreationOptions {
  const user = pk.user as Json;
  return {
    ...(pk as object),
    challenge: base64urlToBuffer(String(pk.challenge)),
    user: { ...(user as object), id: base64urlToBuffer(String(user.id)) },
    excludeCredentials: descriptors(pk.excludeCredentials),
  } as PublicKeyCredentialCreationOptions;
}

/** Manual JSON → `PublicKeyCredentialRequestOptions`. */
export function decodeRequestOptions(pk: Json): PublicKeyCredentialRequestOptions {
  return {
    ...(pk as object),
    challenge: base64urlToBuffer(String(pk.challenge)),
    allowCredentials: descriptors(pk.allowCredentials),
  } as PublicKeyCredentialRequestOptions;
}

/** API creation options → `navigator.credentials.create` argument (native parser when present). */
export function toCreationOptions(options: unknown, signal?: AbortSignal): CredentialCreationOptions {
  const pk = unwrapPublicKey(options);
  let publicKey: PublicKeyCredentialCreationOptions | undefined;
  const native = nativeJson()?.parseCreationOptionsFromJSON;
  if (native) {
    try {
      publicKey = native.call(window.PublicKeyCredential, pk);
    } catch {
      publicKey = undefined; // unknown member or older engine quirk: fall back
    }
  }
  return { publicKey: publicKey ?? decodeCreationOptions(pk), ...(signal ? { signal } : {}) };
}

/** API request options → `navigator.credentials.get` argument (native parser when present). */
export function toRequestOptions(options: unknown, signal?: AbortSignal): CredentialRequestOptions {
  const pk = unwrapPublicKey(options);
  let publicKey: PublicKeyCredentialRequestOptions | undefined;
  const native = nativeJson()?.parseRequestOptionsFromJSON;
  if (native) {
    try {
      publicKey = native.call(window.PublicKeyCredential, pk);
    } catch {
      publicKey = undefined;
    }
  }
  return { publicKey: publicKey ?? decodeRequestOptions(pk), ...(signal ? { signal } : {}) };
}

/**
 * Checks the relying-party id against the page before prompting: a mismatch (for example the app
 * opened as http://127.0.0.1:3000 while the API's RP id is `localhost`) makes the browser fail
 * with an opaque SecurityError, so say what to do instead.
 */
export function rpIdMismatch(options: unknown, hostname: string): string | null {
  const pk = unwrapPublicKey(options);
  const rpId = String((pk.rp as Json | undefined)?.id ?? pk.rpId ?? "");
  if (!rpId) return null;
  const host = hostname.toLowerCase();
  if (host === rpId || host.endsWith(`.${rpId}`)) return null;
  return `Passkeys for this server belong to “${rpId}”, but this page is on “${hostname}”. Open the app at http://${rpId}${typeof window !== "undefined" && window.location.port ? `:${window.location.port}` : ""} and try again.`;
}

interface AttestationLike {
  clientDataJSON: ArrayBuffer;
  attestationObject: ArrayBuffer;
  getTransports?: () => string[];
}
interface AssertionLike {
  clientDataJSON: ArrayBuffer;
  authenticatorData: ArrayBuffer;
  signature: ArrayBuffer;
  userHandle: ArrayBuffer | null;
}
export interface CredentialLike {
  id: string;
  rawId: ArrayBuffer;
  type: string;
  response: AttestationLike | AssertionLike;
  authenticatorAttachment?: string | null;
  getClientExtensionResults?: () => unknown;
  toJSON?: () => unknown;
}

/** Manual `PublicKeyCredential` (registration or assertion) → JSON the API accepts. */
export function encodeCredential(cred: CredentialLike): Json {
  const r = cred.response;
  const response: Json = { clientDataJSON: bufferToBase64url(r.clientDataJSON) };
  if ("attestationObject" in r) {
    response.attestationObject = bufferToBase64url(r.attestationObject);
    response.transports = r.getTransports?.() ?? [];
  } else {
    response.authenticatorData = bufferToBase64url(r.authenticatorData);
    response.signature = bufferToBase64url(r.signature);
    response.userHandle = r.userHandle ? bufferToBase64url(r.userHandle) : null;
  }
  return {
    id: cred.id,
    rawId: bufferToBase64url(cred.rawId),
    type: cred.type,
    response,
    extensions: cred.getClientExtensionResults?.() ?? {},
  };
}

/**
 * `PublicKeyCredential` → JSON the API accepts: the browser's own `toJSON()` when available
 * (its `clientExtensionResults` is renamed to `extensions`, the webauthn-rs field name; extra
 * members such as `authenticatorAttachment` or `response.publicKey` are ignored by the server),
 * otherwise [`encodeCredential`].
 */
export function credentialToJSON(cred: CredentialLike): Json {
  if (typeof cred.toJSON === "function") {
    try {
      const j = cred.toJSON() as Json;
      if (j && typeof j === "object" && typeof j.rawId === "string" && j.response && typeof j.response === "object") {
        // webauthn-rs accepts `clientExtensionResults` as an alias of `extensions`, but not both.
        const { clientExtensionResults, ...rest } = j;
        return { ...rest, extensions: clientExtensionResults ?? rest.extensions ?? {} };
      }
    } catch {
      /* fall through to the manual encoder */
    }
  }
  return encodeCredential(cred);
}

/** Thrown when the user pressed Cancel in our UI (the ceremony was aborted). */
export class PasskeyCancelled extends Error {
  constructor() {
    super("Passkey request cancelled.");
    this.name = "PasskeyCancelled";
  }
}

/** Plain-language message for a failed ceremony (DOMException names from the WebAuthn spec). */
export function passkeyErrorMessage(e: unknown, what: "create" | "get" = "create"): string {
  if (e instanceof PasskeyCancelled) return "Cancelled — nothing was changed.";
  const name = e instanceof DOMException || (e instanceof Error && e.name) ? (e as Error).name : "";
  switch (name) {
    case "AbortError":
      return "Cancelled — nothing was changed.";
    case "NotAllowedError":
      return what === "create"
        ? "The passkey prompt was cancelled or timed out. On Windows, choose Windows Hello (PIN, face or fingerprint); if it isn’t offered, set up a PIN in Settings → Accounts → Sign-in options."
        : "The passkey prompt was cancelled or timed out.";
    case "InvalidStateError":
      return what === "create" ? "This device already has a passkey for your account." : "That passkey can’t be used here.";
    case "SecurityError":
      return "The browser refused the passkey for this address. Open the app at http://localhost (the configured passkey domain) and try again.";
    case "NotSupportedError":
      return "This browser or device doesn’t support the requested passkey type.";
    case "ConstraintError":
      return "This authenticator can’t verify you (no PIN or biometric set up).";
    default:
      return e instanceof Error && e.message ? e.message : "The passkey request failed.";
  }
}

function abortReason(signal?: AbortSignal): never | void {
  if (signal?.aborted) throw new PasskeyCancelled();
}

/** Runs `navigator.credentials.create` for API options; returns the JSON credential. */
export async function createPasskey(options: unknown, signal?: AbortSignal): Promise<WireCredential> {
  const mismatch = typeof window !== "undefined" ? rpIdMismatch(options, window.location.hostname) : null;
  if (mismatch) throw new Error(mismatch);
  abortReason(signal);
  let cred: CredentialLike | null;
  try {
    cred = (await navigator.credentials.create(toCreationOptions(options, signal))) as unknown as CredentialLike | null;
  } catch (e) {
    if (signal?.aborted) throw new PasskeyCancelled();
    throw e;
  }
  if (!cred) throw new Error("No passkey was created");
  return credentialToJSON(cred) as WireCredential;
}

/** Runs `navigator.credentials.get` for API options; returns the JSON assertion. */
export async function getPasskeyAssertion(options: unknown, signal?: AbortSignal): Promise<WireCredential> {
  const mismatch = typeof window !== "undefined" ? rpIdMismatch(options, window.location.hostname) : null;
  if (mismatch) throw new Error(mismatch);
  abortReason(signal);
  let cred: CredentialLike | null;
  try {
    cred = (await navigator.credentials.get(toRequestOptions(options, signal))) as unknown as CredentialLike | null;
  } catch (e) {
    if (signal?.aborted) throw new PasskeyCancelled();
    throw e;
  }
  if (!cred) throw new Error("No passkey was used");
  return credentialToJSON(cred) as WireCredential;
}
