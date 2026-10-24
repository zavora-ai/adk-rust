import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { HttpClient } from "./client";
import "./styles/fonts.css";
import "./styles/tokens.css";
import "./styles/app.css";

const client = new HttpClient();
const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App client={client} seams={{ devices: client.devices() }} takeOver={client.takeOver()} />
    </StrictMode>,
  );
}
