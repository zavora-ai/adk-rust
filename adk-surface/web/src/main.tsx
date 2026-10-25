import { StrictMode } from "react";
import type { ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { HttpClient } from "./client";
import type { Shell } from "./route";
import { shellOf } from "./route";
import { TrayPanel } from "./tray/TrayPanel";
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
      return <App client={client} seams={seams} takeOver={client.takeOver()} />;
    }
    case "tray":
      return <TrayPanel client={new HttpClient("/api", "tray")} />;
  }
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(<StrictMode>{shell(shellOf(window.location))}</StrictMode>);
}
