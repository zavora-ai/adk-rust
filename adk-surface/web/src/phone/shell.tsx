import type { ReactNode } from "react";
import { HttpPhoneClient } from "./client";
import { idbTokens } from "./idb";
import { PhoneApp } from "./PhoneApp";
import { standalone } from "./route";

/** Whether the device's main pointer is a finger: a phone or a tablet, not a computer. */
function touchFirst(): boolean {
  return window.matchMedia?.("(pointer: coarse)").matches === true;
}

/**
 * The phone's shell (spec 3.3 §5.4) when the page opens a pairing link, runs as the app
 * installed on a phone, or this browser holds a device token; `undefined` for every other
 * page, which the home's shells serve — the home installed on a computer included (§5.1).
 */
export async function phoneShell(location: Pick<Location, "pathname">): Promise<ReactNode | undefined> {
  const pairing = location.pathname.startsWith("/ui/pair");
  const installedOnAPhone = standalone() && touchFirst();
  if (!pairing && !installedOnAPhone && (await idbTokens.read()) === undefined) return undefined;
  return <PhoneApp client={await HttpPhoneClient.open(idbTokens)} />;
}
