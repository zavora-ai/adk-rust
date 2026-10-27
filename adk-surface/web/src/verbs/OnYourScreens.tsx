/**
 * "On your screens" in Memory & rules (spec 3.3 §2 item 7, §5.1): the screen actions agents
 * may ask for, each "Agents always ask you first", and the fixed callout that it can't be
 * switched off. Absent when the server has none.
 */
import type { VerbView } from "../generated/verbs";
import { verbLabels } from "./labels";

export function OnYourScreens({ verbs }: { verbs: VerbView[] }) {
  // Only what changes something is listed: reading what is coming up asks no one.
  const asked = verbs.filter((verb) => verb.agentsAsk && verb.effect !== "read");
  if (asked.length === 0) return null;
  return (
    <>
      <p className="card-label">{verbLabels.onYourScreens}</p>
      <ul className="bordered on-your-screens">
        {asked.map((verb) => (
          <li key={verb.name} className="screens-row">
            <span>{verb.description}</span>
            <span className="screens-sub">{verbLabels.agentsAlwaysAskYouFirst}</span>
          </li>
        ))}
      </ul>
      <p className="always-asks">{verbLabels.asksYouFirst}</p>
    </>
  );
}
