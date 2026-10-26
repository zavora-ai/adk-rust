/**
 * "Confirm it's you" for a change that adds an agent or lets one do more at high risk (spec 3.3
 * §7.2): the server binds a challenge to exactly this command, the person's passkey on this
 * computer answers it, and the server makes the change with presence confirmed. The plain
 * command route never makes such a change.
 */
import { ApiError } from "../client";
import type { CommandOutcome, ConfirmOptions } from "../generated/api";
import { phoneLabels } from "../phone/labels";
import type { Post } from "../phone/passkey";
import { credentialJson, requestOptions } from "../phone/passkey";
import type { AgentCommand, AgentOutcome } from "./types";

type Json = Parameters<typeof requestOptions>[0];

export async function confirmAgentChange(post: Post, command: AgentCommand): Promise<AgentOutcome> {
  const options = await post<ConfirmOptions>("/surface/agents/confirm/options", { command });
  const credential = await navigator.credentials
    .get({ publicKey: requestOptions(options.publicKey as Json) })
    .catch((error: unknown) => {
      if (error instanceof DOMException) {
        throw new ApiError(403, phoneLabels.passkeyNotUsed, "surface.phone.passkey_not_used");
      }
      throw error;
    });
  if (!credential) throw new ApiError(403, phoneLabels.passkeyNotUsed, "surface.phone.passkey_not_used");
  const outcome = await post<CommandOutcome>("/surface/agents/confirm", {
    challengeId: options.challengeId,
    credential: credentialJson(credential as PublicKeyCredential),
  });
  return outcome as AgentOutcome;
}
