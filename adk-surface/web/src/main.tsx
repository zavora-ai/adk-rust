import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { HttpClient } from "./client";
import "./styles/fonts.css";
import "./styles/tokens.css";
import "./styles/app.css";

const client = new HttpClient();
// The kernel routes the home reads and changes beside the engine's (spec 3.3 §10 K1–K4).
const seams = { devices: client.devices(), whileAway: client.whileAway(), limits: client.limits() };
const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App client={client} seams={seams} takeOver={client.takeOver()} />
    </StrictMode>,
  );
}
