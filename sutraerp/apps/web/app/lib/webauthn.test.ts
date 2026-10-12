import { afterEach, describe, expect, it, vi } from "vitest";
import {
  base64urlToBuffer,
  bufferToBase64url,
  createPasskey,
  credentialToJSON,
  getPasskeyAssertion,
  PasskeyCancelled,
  passkeyErrorMessage,
  rpIdMismatch,
  toCreationOptions,
  toRequestOptions,
  unwrapPublicKey,
} from "./webauthn";

const bytes = (...b: number[]) => new Uint8Array(b).buffer;

describe("WebAuthn wire helpers", () => {
  it("round-trips base64url without padding", () => {
    const raw = bytes(0xfb, 0xff, 0x00, 0x10, 0x3e);
    const enc = bufferToBase64url(raw);
    expect(enc).toBe("-_8AED4");
    expect(new Uint8Array(base64urlToBuffer(enc))).toEqual(new Uint8Array(raw));
  });

  it("decodes the binary members of creation and request options", () => {
    const create = toCreationOptions({
      publicKey: {
        challenge: "AQID",
        rp: { id: "localhost", name: "SutraERP" },
        user: { id: "BAU", name: "admin@sutra.dev", displayName: "Admin" },
        excludeCredentials: [{ type: "public-key", id: "Bgc" }],
        pubKeyCredParams: [{ type: "public-key", alg: -7 }],
      },
    }).publicKey;
    expect(new Uint8Array(create?.challenge as ArrayBuffer)).toEqual(new Uint8Array([1, 2, 3]));
    expect(new Uint8Array(create?.user.id as ArrayBuffer)).toEqual(new Uint8Array([4, 5]));
    expect(new Uint8Array(create?.excludeCredentials?.[0]?.id as ArrayBuffer)).toEqual(new Uint8Array([6, 7]));
    expect(create?.rp.id).toBe("localhost");

    const get = toRequestOptions({ publicKey: { challenge: "CQ", rpId: "localhost", allowCredentials: [{ type: "public-key", id: "Cg" }] } }).publicKey;
    expect(new Uint8Array(get?.challenge as ArrayBuffer)).toEqual(new Uint8Array([9]));
    expect(new Uint8Array(get?.allowCredentials?.[0]?.id as ArrayBuffer)).toEqual(new Uint8Array([10]));
  });

  it("encodes registration and assertion responses", () => {
    const reg = credentialToJSON({
      id: "abc",
      rawId: bytes(1),
      type: "public-key",
      response: { clientDataJSON: bytes(2), attestationObject: bytes(3), getTransports: () => ["internal"] },
    });
    expect(reg).toEqual({
      id: "abc",
      rawId: "AQ",
      type: "public-key",
      response: { clientDataJSON: "Ag", attestationObject: "Aw", transports: ["internal"] },
      extensions: {},
    });
    const assertion = credentialToJSON({
      id: "abc",
      rawId: bytes(1),
      type: "public-key",
      response: { clientDataJSON: bytes(2), authenticatorData: bytes(4), signature: bytes(5), userHandle: null },
    });
    expect(assertion.response).toEqual({ clientDataJSON: "Ag", authenticatorData: "BA", signature: "BQ", userHandle: null });
  });
});

describe("WebAuthn encoding edge cases", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    // jsdom has no WebAuthn: remove what a test installed.
    delete (window as unknown as { PublicKeyCredential?: unknown }).PublicKeyCredential;
  });

  it("accepts padded base64url, rejects standard base64 characters, handles empty input", () => {
    expect(new Uint8Array(base64urlToBuffer("AQID=="))).toEqual(new Uint8Array([1, 2, 3]));
    expect(() => base64urlToBuffer("a+b/")).toThrow(/base64url/);
    expect(base64urlToBuffer("").byteLength).toBe(0);
    expect(bufferToBase64url(new Uint8Array([]))).toBe("");
    // Views are encoded over their own window of the buffer.
    const view = new Uint8Array([9, 0xfb, 0xff, 9]).subarray(1, 3);
    expect(bufferToBase64url(view)).toBe("-_8");
    // All 256 byte values survive the round trip (32-byte challenge sized chunks).
    const all = new Uint8Array(256).map((_, i) => i);
    expect(new Uint8Array(base64urlToBuffer(bufferToBase64url(all)))).toEqual(all);
  });

  it("unwraps { publicKey } and bare options, drops nulls, requires a challenge", () => {
    const pk = unwrapPublicKey({ publicKey: { challenge: "AQ", hints: null, authenticatorSelection: { authenticatorAttachment: null, residentKey: "preferred" } } });
    expect(pk).toEqual({ challenge: "AQ", authenticatorSelection: { residentKey: "preferred" } });
    expect(unwrapPublicKey({ challenge: "AQ", rpId: "localhost" })).toEqual({ challenge: "AQ", rpId: "localhost" });
    expect(() => unwrapPublicKey({ publicKey: {} })).toThrow(/challenge/);
    expect(() => unwrapPublicKey(null)).toThrow();
  });

  it("prefers the browser's JSON parsers and falls back when they throw", () => {
    const parseCreationOptionsFromJSON = vi.fn(() => ({ native: true }));
    const parseRequestOptionsFromJSON = vi.fn(() => {
      throw new TypeError("unknown member");
    });
    Object.assign(window, { PublicKeyCredential: Object.assign(function PublicKeyCredential() {}, { parseCreationOptionsFromJSON, parseRequestOptionsFromJSON }) });
    const signal = new AbortController().signal;
    const create = toCreationOptions({ publicKey: { challenge: "AQ", user: { id: "Ag", name: "a", displayName: "A" }, hints: null } }, signal);
    expect(create.publicKey).toEqual({ native: true });
    expect(create.signal).toBe(signal);
    expect(parseCreationOptionsFromJSON).toHaveBeenCalledWith({ challenge: "AQ", user: { id: "Ag", name: "a", displayName: "A" } });
    const get = toRequestOptions({ publicKey: { challenge: "CQ", allowCredentials: [{ type: "public-key", id: "Cg", transports: ["internal"] }] } });
    expect(parseRequestOptionsFromJSON).toHaveBeenCalled();
    expect(new Uint8Array(get.publicKey?.challenge as ArrayBuffer)).toEqual(new Uint8Array([9]));
    expect(get.publicKey?.allowCredentials?.[0]?.transports).toEqual(["internal"]);
  });

  it("uses credential.toJSON() and renames clientExtensionResults to extensions", () => {
    const json = credentialToJSON({
      id: "abc",
      rawId: bytes(1),
      type: "public-key",
      response: { clientDataJSON: bytes(2), attestationObject: bytes(3) },
      toJSON: () => ({
        id: "abc",
        rawId: "AQ",
        type: "public-key",
        authenticatorAttachment: "platform",
        response: { clientDataJSON: "Ag", attestationObject: "Aw", transports: ["internal"], publicKeyAlgorithm: -7 },
        clientExtensionResults: { credProps: { rk: true } },
      }),
    });
    expect(json.extensions).toEqual({ credProps: { rk: true } });
    expect("clientExtensionResults" in json).toBe(false);
    expect(json.rawId).toBe("AQ");
    // A broken toJSON falls back to the manual encoder.
    const fallback = credentialToJSON({
      id: "abc",
      rawId: bytes(1),
      type: "public-key",
      response: { clientDataJSON: bytes(2), authenticatorData: bytes(4), signature: bytes(5), userHandle: bytes(6) },
      toJSON: () => {
        throw new Error("nope");
      },
    });
    expect(fallback.response).toEqual({ clientDataJSON: "Ag", authenticatorData: "BA", signature: "BQ", userHandle: "Bg" });
  });

  it("detects a relying-party id that does not match the page host", () => {
    const opts = { publicKey: { challenge: "AQ", rp: { id: "localhost", name: "SutraERP" } } };
    expect(rpIdMismatch(opts, "localhost")).toBeNull();
    expect(rpIdMismatch({ publicKey: { challenge: "AQ", rpId: "sutra.dev" } }, "erp.sutra.dev")).toBeNull();
    expect(rpIdMismatch(opts, "127.0.0.1")).toMatch(/localhost/);
  });

  it("maps WebAuthn DOMExceptions to plain messages", () => {
    expect(passkeyErrorMessage(new DOMException("x", "NotAllowedError"))).toMatch(/Windows Hello/);
    expect(passkeyErrorMessage(new DOMException("x", "NotAllowedError"), "get")).toMatch(/cancelled or timed out/);
    expect(passkeyErrorMessage(new DOMException("x", "InvalidStateError"))).toMatch(/already has a passkey/);
    expect(passkeyErrorMessage(new DOMException("x", "SecurityError"))).toMatch(/localhost/);
    expect(passkeyErrorMessage(new PasskeyCancelled())).toMatch(/Cancelled/);
    expect(passkeyErrorMessage(new DOMException("x", "AbortError"))).toMatch(/Cancelled/);
  });

  it("aborting a pending ceremony rejects with PasskeyCancelled", async () => {
    Object.assign(window, { PublicKeyCredential: function PublicKeyCredential() {} });
    const pending = (opts: { signal?: AbortSignal }) =>
      new Promise((_, reject) => opts.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError"))));
    vi.stubGlobal("navigator", { credentials: { create: vi.fn(pending), get: vi.fn(pending) } });
    const abort = new AbortController();
    const create = createPasskey({ publicKey: { challenge: "AQ", rp: { id: "localhost" }, user: { id: "Ag", name: "a", displayName: "A" } } }, abort.signal);
    abort.abort();
    await expect(create).rejects.toBeInstanceOf(PasskeyCancelled);
    const abort2 = new AbortController();
    const get = getPasskeyAssertion({ publicKey: { challenge: "AQ", rpId: "localhost" } }, abort2.signal);
    abort2.abort();
    await expect(get).rejects.toBeInstanceOf(PasskeyCancelled);
    // Already aborted: never prompts.
    await expect(createPasskey({ publicKey: { challenge: "AQ", user: { id: "Ag" } } }, abort.signal)).rejects.toBeInstanceOf(PasskeyCancelled);
  });

  it("refuses to prompt when the page host is not the relying party", async () => {
    const create = vi.fn();
    vi.stubGlobal("navigator", { credentials: { create } });
    await expect(createPasskey({ publicKey: { challenge: "AQ", rp: { id: "sutra.example" }, user: { id: "Ag" } } })).rejects.toThrow(/sutra\.example/);
    expect(create).not.toHaveBeenCalled();
  });
});
