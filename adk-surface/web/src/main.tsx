import { StrictMode } from "react";
import type { ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { HttpClient } from "./client";
import { phoneShell } from "./phone/shell";
import type { Shell } from "./route";
import { shellOf } from "./route";
import { TrayPanel } from "./tray/TrayPanel";
import { HttpVerbsClient } from "./verbs/client";
import { HttpWebsiteClient } from "./website/client";
import { HttpWorkflowClient } from "./workflow/client";
import "./styles/fonts.css";
import "./styles/tokens.css";
import "./styles/app.css";

/** One page, one shell per address (spec 3.3 §5): each shell builds its own client. */
function shell(kind: Shell): ReactNode {
  switch (kind) {
    case "home": {
      const client = new HttpClient();
      // The kernel routes the home reads and changes beside the engine's (spec 3.3 §10 K1–K5).
      const seams = {
        devices: client.devices(),
        whileAway: client.whileAway(),
        limits: client.limits(),
        memory: client.memory(),
        record: client.record(),
        roster: client.roster(),
      };
      // The website's views, where the server serves a website (spec 3.5 §8.2).
      const website = new HttpWebsiteClient("/api/surface/website", (token) => client.signIn(token));
      // Screen actions, where the server takes them (spec 3.3 §2 item 7).
      const verbs = new HttpVerbsClient();
      // The workflow observer's Runs tab, where the server observes graph runs (spec 3.3 §12).
      const workflow = new HttpWorkflowClient();
      return (
        <App
          client={client}
          seams={seams}
          takeOver={client.takeOver()}
          website={website}
          verbs={verbs}
          workflow={workflow}
        />
      );
    }
    case "tray":
      return <TrayPanel client={new HttpClient("/api", "tray")} />;
  }
}

const root = document.getElementById("root");
if (root) {
  void phoneShell(window.location).then((phone) =>
    createRoot(root).render(<StrictMode>{phone ?? shell(shellOf(window.location))}</StrictMode>),
  );
}
