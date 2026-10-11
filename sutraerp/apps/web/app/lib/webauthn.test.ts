import { describe, expect, it } from "vitest";
import { base64urlToBuffer, bufferToBase64url, credentialToJSON, toCreationOptions, toRequestOptions } from "./webauthn";

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
