// The screens over the example of spec 3.3 §5.1, for `npm run dev` at /ui/fixture.html and
// for the screenshots beside the spec's images. Not part of the built page.
import { StrictMode } from "react";
import type { ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "../App";
import type { Section } from "../route";
import { TrayPanel } from "../tray/TrayPanel";
import { FixtureClient, fixtureSeams, takeOverCard } from "./fixture";
import { TrayFixture } from "./tray";
import "../styles/fonts.css";
import "../styles/tokens.css";
import "../styles/app.css";

const params = new URLSearchParams(window.location.search);
const section = (params.get("section") ?? "threads") as Section;
const client = new FixtureClient();
// `?takeOver` adds Household's take over (spec 3.3 §8.2) to the example.
if (params.has("takeOver")) client.hold(takeOverCard());
// `?screen=` shows another surface's screen over its own example: one line per surface.
const screens: Record<string, () => ReactNode> = {
  tray: () => <TrayPanel client={new TrayFixture()} />,
};
const screen = screens[params.get("screen") ?? ""];
const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      {screen ? (
        screen()
      ) : (
        <App
          client={client}
          seams={fixtureSeams()}
          takeOver={{ provide: (card) => client.takeOver(card) }}
          initialSection={section}
        />
      )}
    </StrictMode>,
  );
}
