/**
 * Browser side of the dev-mode WebAuthn ceremonies (passkeys / security keys).
 *
 * The API sends `PublicKeyCredentialCreationOptions` / `…RequestOptions` as JSON with binary
 * members base64url encoded (webauthn-rs wire format, under `publicKey`). The browser API needs
 * `ArrayBuffer`s, and its responses must go back base64url encoded. These helpers do both ways
 * and contain no other logic: all verification happens on the server.
 */

type Json = Record<string, unknown>;

/**
 * A credential as the generated API types expect it: the OpenAPI schema declares WebAuthn JSON
 * as an opaque `object`, which openapi-typescript renders as `Record<string, never>`.
 */
export type WireCredential = Record<string, never>;

export function base64urlToBuffer(value: string): ArrayBuffer {
  const b64 = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
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

function descriptors(list: unknown): PublicKeyCredentialDescriptor[] | undefined {
  if (!Array.isArray(list)) return undefined;
  return list.map((d: Json) => ({ ...(d as object), id: base64urlToBuffer(String(d.id)) }) as PublicKeyCredentialDescriptor);
}

/** API creation options → `navigator.credentials.create` argument. */
export function toCreationOptions(options: unknown): CredentialCreationOptions {
  const pk = (options as Json).publicKey as Json;
  const user = pk.user as Json;
  return {
    publicKey: {
      ...(pk as object),
      challenge: base64urlToBuffer(String(pk.challenge)),
      user: { ...(user as object), id: base64urlToBuffer(String(user.id)) },
      excludeCredentials: descriptors(pk.excludeCredentials),
    } as PublicKeyCredentialCreationOptions,
  };
}

/** API request options → `navigator.credentials.get` argument. */
export function toRequestOptions(options: unknown): CredentialRequestOptions {
  const pk = (options as Json).publicKey as Json;
  return {
    publicKey: {
      ...(pk as object),
      challenge: base64urlToBuffer(String(pk.challenge)),
      allowCredentials: descriptors(pk.allowCredentials),
    } as PublicKeyCredentialRequestOptions,
  };
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
  getClientExtensionResults?: () => unknown;
}

/** `PublicKeyCredential` (registration or assertion) → JSON the API accepts. */
export function credentialToJSON(cred: CredentialLike): Json {
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

/** Runs `navigator.credentials.create` for API options; returns the JSON credential. */
export async function createPasskey(options: unknown): Promise<WireCredential> {
  const cred = (await navigator.credentials.create(toCreationOptions(options))) as unknown as CredentialLike | null;
  if (!cred) throw new Error("No passkey was created");
  return credentialToJSON(cred) as WireCredential;
}

/** Runs `navigator.credentials.get` for API options; returns the JSON assertion. */
export async function getPasskeyAssertion(options: unknown): Promise<WireCredential> {
  const cred = (await navigator.credentials.get(toRequestOptions(options))) as unknown as CredentialLike | null;
  if (!cred) throw new Error("No passkey was used");
  return credentialToJSON(cred) as WireCredential;
}
