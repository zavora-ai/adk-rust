import { useId, useState } from "react";
import type { FormEvent } from "react";
import type { SetupView } from "../generated/website";
import { problemText } from "../client";
import type { WebsiteClient } from "./client";
import { websiteLabels as labels } from "./labels";
import { Display } from "./parts";

/**
 * The setup code (spec 3.5 §7.17, A-I20): a fresh site takes no onboarding input until the
 * code from its logs pairs the owner's device. The token it gives stays in this tab.
 */
export function SetupCode({
  client,
  setup,
  onClaimed,
}: {
  client: WebsiteClient;
  setup: SetupView;
  onClaimed: () => void;
}) {
  const [code, setCode] = useState("");
  const [device, setDevice] = useState(labels.setupDeviceDefault);
  const [problem, setProblem] = useState("");
  const [busy, setBusy] = useState(false);
  const codeId = useId();
  const deviceId = useId();

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setProblem("");
    try {
      await client.claim(code.trim(), device.trim());
      onClaimed();
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="w-page w-setup glow">
      <Display lead={labels.setupLead} accent={labels.setupAccent}>
        {labels.setupSubhead}
      </Display>
      {setup.paired ? (
        <p className="card w-setup-card">{labels.pairedElsewhere}</p>
      ) : (
        <form className="card w-setup-card" onSubmit={(event) => void submit(event)}>
          <h2 className="card-heading">{setup.siteName}</h2>
          <p className="muted">{setup.whereCode}</p>
          <div className="w-field">
            <label htmlFor={codeId}>{labels.setupCode}</label>
            <input
              id={codeId}
              className="w-code-input mono"
              autoComplete="one-time-code"
              autoCapitalize="characters"
              spellCheck={false}
              value={code}
              onChange={(event) => setCode(event.target.value)}
            />
          </div>
          <div className="w-field">
            <label htmlFor={deviceId}>{labels.setupDevice}</label>
            <input id={deviceId} value={device} onChange={(event) => setDevice(event.target.value)} />
          </div>
          {problem && (
            <p className="decision-problem" role="alert">
              {problem}
            </p>
          )}
          <button type="submit" className="button button-primary" disabled={busy || !code.trim()}>
            {labels.claim}
          </button>
        </form>
      )}
      <p className="w-holding">
        <span className="muted">{labels.visitorsSee} </span>
        {setup.holding}
      </p>
    </main>
  );
}
