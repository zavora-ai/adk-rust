/**
 * WebAuthn's JSON forms and the browser's binary ones (spec 3.3 §8.2): the server sends
 * options with base64url fields; `navigator.credentials` takes and gives ArrayBuffers.
 */
import { ApiError } from "../client";
import type { CommandOutcome, ConfirmOptions, ConfirmOptionsRequest, PasskeyOptions } from "../generated/api";
import { phoneLabels } from "./labels";

export function fromBase64url(text: string): ArrayBuffer {
  const base64 = text.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(text.length / 4) * 4, "=");
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes.buffer;
}

export function toBase64url(buffer: ArrayBuffer | ArrayBufferView): string {
  const bytes = buffer instanceof ArrayBuffer ? new Uint8Array(buffer) : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

type Json = Record<string, unknown>;
type Descriptor = { type: string; id: string; transports?: string[] };

function descriptors(list: unknown): PublicKeyCredentialDescriptor[] | undefined {
  if (!Array.isArray(list)) return undefined;
  return (list as Descriptor[]).map((item) => ({
    type: "public-key",
    id: fromBase64url(item.id),
    ...(item.transports ? { transports: item.transports as AuthenticatorTransport[] } : {}),
  }));
}

/** `PublicKeyCredentialCreationOptionsJSON` as `navigator.credentials.create` takes it. */
export function creationOptions(json: Json): PublicKeyCredentialCreationOptions {
  const user = json.user as Json;
  return {
    ...(json as unknown as PublicKeyCredentialCreationOptions),
    challenge: fromBase64url(json.challenge as string),
    user: { ...(user as unknown as PublicKeyCredentialUserEntity), id: fromBase64url(user.id as string) },
    excludeCredentials: descriptors(json.excludeCredentials),
  };
}

/** `PublicKeyCredentialRequestOptionsJSON` as `navigator.credentials.get` takes it. */
export function requestOptions(json: Json): PublicKeyCredentialRequestOptions {
  return {
    ...(json as unknown as PublicKeyCredentialRequestOptions),
    challenge: fromBase64url(json.challenge as string),
    allowCredentials: descriptors(json.allowCredentials),
  };
}

/** A credential as the server reads it: `credential.toJSON()`, or the same built by hand. */
export function credentialJson(credential: PublicKeyCredential): Json {
  const own = credential as PublicKeyCredential & { toJSON?: () => Json };
  if (typeof own.toJSON === "function") return own.toJSON();
  const response = credential.response as AuthenticatorAttestationResponse & AuthenticatorAssertionResponse;
  const encoded: Json = { clientDataJSON: toBase64url(response.clientDataJSON) };
  if ("attestationObject" in response && response.attestationObject) {
    encoded.attestationObject = toBase64url(response.attestationObject);
    encoded.transports = response.getTransports?.() ?? [];
  }
  if ("authenticatorData" in response && response.authenticatorData) {
    encoded.authenticatorData = toBase64url(response.authenticatorData);
    encoded.signature = toBase64url(response.signature);
    encoded.userHandle = response.userHandle ? toBase64url(response.userHandle) : null;
  }
  return {
    id: credential.id,
    rawId: toBase64url(credential.rawId),
    type: credential.type,
    response: encoded,
    clientExtensionResults: credential.getClientExtensionResults?.() ?? {},
    authenticatorAttachment: credential.authenticatorAttachment ?? null,
  };
}

/** Whether this browser can confirm it is you with a passkey on this device. */
export async function platformPasskeys(): Promise<boolean> {
  try {
    return (
      typeof PublicKeyCredential !== "undefined" &&
      (await PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable())
    );
  } catch {
    return false;
  }
}

/** A JSON `POST` to the surface API, resolving to the parsed answer; each client has its own. */
export type Post = <T>(path: string, body?: unknown) => Promise<T>;

/**
 * What the browser threw when the person cancelled or the passkey could not be used, as the
 * person reads it; any other failure is passed on.
 */
function notUsed(message: string) {
  return (error: unknown): never => {
    if (error instanceof DOMException) throw new ApiError(403, message, "surface.phone.passkey_not_used");
    throw error;
  };
}

/**
 * Sets up a passkey on this device through the phone routes (spec 3.3 §8.2): the server's
 * options, `navigator.credentials.create`, then the registration the server verifies. A phone
 * registers its own; the home registers the person's, on this computer.
 */
export async function registerPasskey(post: Post): Promise<void> {
  const options = await post<PasskeyOptions>("/surface/phone/passkey/options");
  const credential = await navigator.credentials
    .create({ publicKey: creationOptions(options.publicKey as Json) })
    .catch(notUsed(phoneLabels.passkeyNotSetUp));
  if (!credential) throw new ApiError(403, phoneLabels.passkeyNotSetUp, "surface.phone.passkey_not_used");
  await post("/surface/phone/passkey", {
    registrationId: options.registrationId,
    credential: credentialJson(credential as PublicKeyCredential),
  });
}

/**
 * Answers decision `id` after the person confirms it is them (spec 3.3 §8.2): a challenge the
 * server binds to this version, answer and edited arguments, the passkey, then the answer the
 * server gives with presence confirmed. It never uses the plain answer route.
 */
export async function confirmAnswer(post: Post, id: string, request: ConfirmOptionsRequest): Promise<CommandOutcome> {
  const path = `/surface/decisions/${encodeURIComponent(id)}/confirm`;
  const options = await post<ConfirmOptions>(`${path}/options`, request);
  const credential = await navigator.credentials
    .get({ publicKey: requestOptions(options.publicKey as Json) })
    .catch(notUsed(phoneLabels.passkeyNotUsed));
  if (!credential) throw new ApiError(403, phoneLabels.passkeyNotUsed, "surface.phone.passkey_not_used");
  return post<CommandOutcome>(path, {
    challengeId: options.challengeId,
    credential: credentialJson(credential as PublicKeyCredential),
  });
}
