import { useEffect, useState } from "react";
import { problemText } from "../client";
import type { PhoneClient } from "./client";
import { phoneLabels } from "./labels";
import { platformPasskeys } from "./passkey";
import { standalone } from "./route";

type Step = "claim" | "notify" | "passkey" | "done";

/** Safari on an iPhone or iPad, where notifications and the app's storage need the Home Screen app. */
function ios(): boolean {
  const agent = navigator.userAgent;
  return /iPhone|iPad|iPod/.test(agent) || (/Macintosh/.test(agent) && navigator.maxTouchPoints > 1);
}

type InstallPrompt = Event & { prompt: () => Promise<void> };

/**
 * Pairing on the phone (spec 3.3 §5.4, WP M step 8): installed first on iOS, where a Home
 * Screen app keeps its storage apart from Safari's; then the code, "Allow notifications" and
 * "Use your face or fingerprint", each started by a tap. Where a passkey cannot be set up,
 * high-risk calls are approved in the home on the computer.
 */
export function Pair({
  client,
  code: given,
  onDone,
}: {
  client: PhoneClient;
  code?: string;
  onDone: () => void;
}) {
  const [step, setStep] = useState<Step>(client.paired() ? "notify" : "claim");
  const [code, setCode] = useState(given ?? "");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [notifyProblem, setNotifyProblem] = useState("");
  const [install, setInstall] = useState<InstallPrompt>();
  const [passkeys, setPasskeys] = useState<boolean>();

  useEffect(() => {
    const offer = (event: Event) => {
      event.preventDefault();
      setInstall(event as InstallPrompt);
    };
    window.addEventListener("beforeinstallprompt", offer);
    void platformPasskeys().then(setPasskeys);
    return () => window.removeEventListener("beforeinstallprompt", offer);
  }, []);

  async function act(work: () => Promise<Step>) {
    setBusy(true);
    setProblem("");
    try {
      setStep(await work());
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  if (step === "claim" && ios() && !standalone()) {
    return (
      <section className="phone-pair" aria-labelledby="phone-pair-heading">
        <h1 id="phone-pair-heading" className="phone-display">
          {phoneLabels.addToHomeScreenFirst}
        </h1>
        <p className="phone-lede">{phoneLabels.iosSteps}</p>
        {code && <p className="phone-code">{code}</p>}
      </section>
    );
  }

  return (
    <section className="phone-pair" aria-labelledby="phone-pair-heading">
      <h1 id="phone-pair-heading" className="phone-display">
        {phoneLabels.pairTitle}
      </h1>
      {install && !standalone() && (
        <button type="button" className="phone-button phone-button-secondary" onClick={() => void install.prompt()}>
          {phoneLabels.install}
        </button>
      )}
      {step === "claim" && (
        <form
          className="phone-form"
          onSubmit={(event) => {
            event.preventDefault();
            void act(async () => {
              await client.claim(code);
              return "notify";
            });
          }}
        >
          <p className="phone-lede">{phoneLabels.pairIntro}</p>
          <label className="phone-label" htmlFor="phone-pair-code">
            {phoneLabels.pairCode}
          </label>
          <input
            id="phone-pair-code"
            className="phone-input"
            autoComplete="one-time-code"
            autoCapitalize="characters"
            value={code}
            onChange={(event) => setCode(event.target.value)}
          />
          <button type="submit" className="phone-button phone-button-primary" disabled={busy || !code.trim()}>
            {phoneLabels.pair}
          </button>
        </form>
      )}
      {step !== "claim" && <p className="phone-done-line">{phoneLabels.paired}</p>}
      {notifyProblem && <p className="phone-note">{phoneLabels.notificationsOff(notifyProblem)}</p>}
      {step === "notify" && (
        <button
          type="button"
          className="phone-button phone-button-primary"
          disabled={busy}
          onClick={() =>
            void act(async () => {
              // Declined, unsupported or refused, the phone still pairs and goes on to its
              // passkey; its Memory tab offers notifications again.
              setNotifyProblem("");
              await client.allowNotifications().catch((error: unknown) => setNotifyProblem(problemText(error)));
              return "passkey";
            })
          }
        >
          {phoneLabels.allowNotifications}
        </button>
      )}
      {step === "passkey" &&
        (passkeys === false ? (
          <>
            <p className="phone-note">{phoneLabels.approveItInTheHome}</p>
            <button type="button" className="phone-button phone-button-primary" onClick={onDone}>
              {phoneLabels.done}
            </button>
          </>
        ) : (
          <button
            type="button"
            className="phone-button phone-button-primary"
            disabled={busy}
            onClick={() =>
              void act(async () => {
                await client.addPasskey();
                return "done";
              })
            }
          >
            {phoneLabels.useYourFaceOrFingerprint}
          </button>
        ))}
      {step === "done" && (
        <>
          <p className="phone-done-line">{phoneLabels.passkeyOn}</p>
          <button type="button" className="phone-button phone-button-primary" onClick={onDone}>
            {phoneLabels.done}
          </button>
        </>
      )}
      {problem && (
        <>
          <p className="phone-problem" role="alert">
            {problem}
          </p>
          {step === "passkey" && <p className="phone-note">{phoneLabels.approveItInTheHome}</p>}
        </>
      )}
    </section>
  );
}
