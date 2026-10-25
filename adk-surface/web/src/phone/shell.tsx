import type { ReactNode } from "react";
import { HttpPhoneClient } from "./client";
import { idbTokens } from "./idb";
import { PhoneApp } from "./PhoneApp";
import { standalone } from "./route";

/**
 * The phone's shell (spec 3.3 §5.4) when the page opens a pairing link, runs as the installed
 * app, or this browser holds a device token; `undefined` for every other page, which the
 * home's shells serve.
 */
export async function phoneShell(location: Pick<Location, "pathname">): Promise<ReactNode | undefined> {
  const pairing = location.pathname.startsWith("/ui/pair");
  if (!pairing && !standalone() && (await idbTokens.read()) === undefined) return undefined;
  return <PhoneApp client={await HttpPhoneClient.open(idbTokens)} />;
}
