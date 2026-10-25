/**
 * Where the shared message box finds voice (spec 3.3 §5.6): set by the app when the server
 * offers voice and the page is in a secure context, absent otherwise.
 */
import { createContext } from "react";

export interface VoiceEntry {
  /** Opens the voice view. */
  open(): void;
}

export const VoiceEntryContext = createContext<VoiceEntry | undefined>(undefined);

/** Voice is offered only where the browser lets a page use the microphone: localhost or https. */
export function voiceOffered(models: number, secure: boolean = globalThis.isSecureContext === true): boolean {
  return secure && models > 0;
}
