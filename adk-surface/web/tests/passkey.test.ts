// WebAuthn's JSON forms and the browser's binary ones (spec 3.3 §8.2).
import { describe, expect, it } from "vitest";
import { creationOptions, credentialJson, fromBase64url, requestOptions, toBase64url } from "../src/phone/passkey";

describe("passkey encoding", () => {
  it("round-trips base64url without padding", () => {
    for (const bytes of [[], [0], [251, 255], [1, 2, 3, 4, 5, 250, 251, 252, 253]]) {
      const text = toBase64url(new Uint8Array(bytes));
      expect(text).not.toMatch(/[+/=]/);
      expect([...new Uint8Array(fromBase64url(text))]).toEqual(bytes);
    }
  });

  it("turns the server's options into the browser's", () => {
    const create = creationOptions({
      challenge: "AQID",
      rp: { id: "localhost", name: "ADK-Rust" },
      user: { id: "BAUG", name: "ana", displayName: "ana" },
      pubKeyCredParams: [{ type: "public-key", alg: -7 }],
      excludeCredentials: [{ type: "public-key", id: "Bwg" }],
    });
    expect([...new Uint8Array(create.challenge as ArrayBuffer)]).toEqual([1, 2, 3]);
    expect([...new Uint8Array(create.user.id as ArrayBuffer)]).toEqual([4, 5, 6]);
    expect([...new Uint8Array(create.excludeCredentials![0].id as ArrayBuffer)]).toEqual([7, 8]);
    const get = requestOptions({ challenge: "CQo", rpId: "localhost", allowCredentials: [{ type: "public-key", id: "Cw" }] });
    expect([...new Uint8Array(get.challenge as ArrayBuffer)]).toEqual([9, 10]);
    expect([...new Uint8Array(get.allowCredentials![0].id as ArrayBuffer)]).toEqual([11]);
  });

  it("writes an assertion as the server reads it when the browser has no toJSON", () => {
    const buffer = (bytes: number[]) => new Uint8Array(bytes).buffer;
    const credential = {
      id: "AQ",
      rawId: buffer([1]),
      type: "public-key",
      authenticatorAttachment: "platform",
      getClientExtensionResults: () => ({}),
      response: {
        clientDataJSON: buffer([2]),
        authenticatorData: buffer([3]),
        signature: buffer([4]),
        userHandle: null,
      },
    } as unknown as PublicKeyCredential;
    expect(credentialJson(credential)).toEqual({
      id: "AQ",
      rawId: "AQ",
      type: "public-key",
      response: { clientDataJSON: "Ag", authenticatorData: "Aw", signature: "BA", userHandle: null },
      clientExtensionResults: {},
      authenticatorAttachment: "platform",
    });
  });
});
