import { useCallback, useEffect, useState } from "react";
import type { PhoneInfo } from "../generated/api";
import { Avatar } from "../components/Avatar";
import { Pill } from "../components/Pill";
import { problemText } from "../client";
import { usePoll } from "../poll";
import { visible } from "../safe";
import type { PhoneClient } from "./client";
import { phoneLabels } from "./labels";

/** Threads on the phone: the agents and what each is doing. Messages are sent from the home. */
export function Agents({ client }: { client: PhoneClient }) {
  const agents = usePoll(() => client.agents(), 10_000);
  return (
    <ul className="phone-rows">
      {(agents.value ?? []).map((row) => (
        <li key={row.agent.name} className="phone-row">
          <Avatar agent={row.agent} size="sm" />
          <strong>{visible(row.agent.displayName)}</strong>
          <span className="phone-row-text">{visible(row.statusText)}</span>
        </li>
      ))}
    </ul>
  );
}

/** Today on the phone: "While you were away", read only. */
export function TodayLines({ client }: { client: PhoneClient }) {
  const briefing = usePoll(() => client.briefing(), 30_000);
  return (
    <section aria-label={phoneLabels.whileYouWereAway}>
      <h2 className="phone-section">{phoneLabels.whileYouWereAway}</h2>
      {briefing.value && <p className="phone-row-text">{briefing.value.sinceText}</p>}
      <ul className="phone-rows">
        {(briefing.value?.lines ?? []).map((line) => (
          <li key={line.seq} className="phone-row">
            <span className="phone-time">{line.atText}</span>
            <span className="phone-row-text">{visible(line.text)}</span>
            {line.pillText && <Pill kind="neutral">{line.pillText}</Pill>}
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * Memory on the phone: this phone's own place in "Where you can say yes from". Pairing, other
 * devices and the operator's rules stay in the home. Notifications the browser or the phone's
 * settings refuse are said, with where to turn them on.
 */
export function ThisPhone({ client, onSignedOut }: { client: PhoneClient; onSignedOut: () => void }) {
  const [info, setInfo] = useState<PhoneInfo>();
  const [problem, setProblem] = useState("");
  const read = useCallback(
    () => client.info().then(setInfo, (error: unknown) => setProblem(problemText(error))),
    [client],
  );
  useEffect(() => {
    void read();
  }, [read]);
  const act = (work: () => Promise<unknown>) => () =>
    void work().then(read, (error: unknown) => setProblem(problemText(error)));
  return (
    <section>
      <ul className="phone-rows">
        <li className="phone-row">
          <strong>{phoneLabels.yourPhone}</strong>
          {info?.passkey && <span className="phone-row-text">{phoneLabels.passkeyOn}</span>}
          {info?.push && <span className="phone-row-text">{phoneLabels.notificationsOn}</span>}
        </li>
      </ul>
      {info && !info.push && (
        <button
          type="button"
          className="phone-button phone-button-secondary"
          onClick={act(async () => {
            setProblem("");
            if (!(await client.allowNotifications())) setProblem(phoneLabels.notificationsOff(phoneLabels.notificationsBlocked));
          })}
        >
          {phoneLabels.allowNotifications}
        </button>
      )}
      {info?.canAddPasskey && (
        <button type="button" className="phone-button phone-button-secondary" onClick={act(() => client.addPasskey())}>
          {phoneLabels.useYourFaceOrFingerprint}
        </button>
      )}
      <button
        type="button"
        className="phone-button phone-button-quiet"
        onClick={() => void client.signOut().then(onSignedOut)}
      >
        {phoneLabels.signOut}
      </button>
      {problem && (
        <p className="phone-problem" role="alert">
          {problem}
        </p>
      )}
    </section>
  );
}
