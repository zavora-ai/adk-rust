import { useCallback, useEffect, useState } from "react";
import type { MouseEvent } from "react";
import type { SetupView, WebsiteHome } from "../generated/website";
import { problemText } from "../client";
import type { WebsiteClient } from "./client";
import { Editor } from "./Editor";
import { websiteLabels as labels } from "./labels";
import { Onboarding } from "./Onboarding";
import { Review } from "./Review";
import { SetupCode } from "./SetupCode";
import { SiteView } from "./SiteView";
import "./website.css";

/** The website's views, each its own address under `/ui/website` (spec 3.5 §8.2). */
export type WebsiteView = "site" | "write" | "review" | "setup";

const BASE = "/ui/website";

export function websiteViewOf(pathname: string): WebsiteView | undefined {
  if (!pathname.startsWith(BASE)) return undefined;
  const rest = pathname.slice(BASE.length).replace(/^\//, "").split("/")[0];
  return rest === "write" || rest === "review" || rest === "setup" ? rest : rest === "" ? undefined : "site";
}

export function websitePathOf(view: WebsiteView): string {
  return view === "site" ? BASE : `${BASE}/${view}`;
}

const tabs: Array<{ view: WebsiteView; label: string }> = [
  { view: "site", label: labels.site },
  { view: "write", label: labels.write },
  { view: "review", label: labels.review },
  { view: "setup", label: labels.setUp },
];

/**
 * The website section of the web home (spec 3.5 §8.2): the site view, the web editor, the
 * weekly review and onboarding. A fresh site first asks for its setup code (A-I20); until it is
 * live, onboarding is what the section opens on.
 */
export function Website({ client, onThreads }: { client: WebsiteClient; onThreads: () => void }) {
  const [home, setHome] = useState<WebsiteHome>();
  const [setup, setSetup] = useState<SetupView>();
  const [problem, setProblem] = useState("");
  const [chosen, setChosen] = useState<WebsiteView | undefined>(() => websiteViewOf(window.location.pathname));

  const load = useCallback(async () => {
    try {
      const value = await client.home();
      setHome(value);
      if (value?.needsSetup) setSetup(await client.setup());
    } catch (error) {
      setProblem(problemText(error));
    }
  }, [client]);
  useEffect(() => {
    void load();
  }, [load]);
  useEffect(() => {
    const back = () => setChosen(websiteViewOf(window.location.pathname));
    window.addEventListener("popstate", back);
    return () => window.removeEventListener("popstate", back);
  }, []);

  const go = (view: WebsiteView) => (event: MouseEvent<HTMLAnchorElement>) => {
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
    event.preventDefault();
    setChosen(view);
    if (window.location.pathname !== websitePathOf(view)) window.history.pushState(null, "", websitePathOf(view));
  };

  if (problem) {
    return (
      <p className="banner" role="alert">
        {problem}
      </p>
    );
  }
  if (!home) return <p className="w-page muted">{labels.loading}</p>;
  if (home.needsSetup && setup) {
    return <SetupCode client={client} setup={setup} onClaimed={() => void load()} />;
  }
  const view = chosen ?? (home.launched ? "site" : "setup");
  return (
    <div className="w-shell">
      <nav className="w-tabs" aria-label={labels.views}>
        <span className="w-site-name">{home.name}</span>
        <div className="segmented w-segmented-4">
          {tabs.map((tab) => (
            <a
              key={tab.view}
              href={websitePathOf(tab.view)}
              className="segment"
              aria-current={view === tab.view ? "true" : undefined}
              onClick={go(tab.view)}
            >
              {tab.label}
            </a>
          ))}
        </div>
      </nav>
      {view === "site" && <SiteView client={client} />}
      {view === "write" && <Editor client={client} />}
      {view === "review" && (
        <Review
          client={client}
          onThreads={onThreads}
          onPromote={async (job) => {
            await client.decideSite({ type: "promote", job });
          }}
        />
      )}
      {view === "setup" && (
        <Onboarding
          client={client}
          onLaunched={() => {
            // The plan for the first 90 days shows before the site view does.
            setChosen("setup");
            void load();
          }}
        />
      )}
    </div>
  );
}
