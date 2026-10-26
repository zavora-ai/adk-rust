// The screens over the example of spec 3.3 §5.1, for `npm run dev` at /ui/fixture.html and
// for the screenshots beside the spec's images. Not part of the built page.
import { StrictMode } from "react";
import type { ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "../App";
import { PhoneApp } from "../phone/PhoneApp";
import type { Section } from "../route";
import { Voice } from "../screens/Voice";
import { TrayPanel } from "../tray/TrayPanel";
import { Website } from "../website/Website";
import { agentsFixture } from "./agents";
import { FixtureClient, fixtureSeams, releaseCard, takeOverCard } from "./fixture";
import { phoneFixture } from "./phone";
import { TrayFixture } from "./tray";
import { VoiceFixtureClient, fixtureDevices } from "./voice";
import { WebsiteFixture } from "./website";
import "../styles/fonts.css";
import "../styles/tokens.css";
import "../styles/app.css";

const params = new URLSearchParams(window.location.search);
const section = (params.get("section") ?? "threads") as Section;
const client = new FixtureClient();
// `?takeOver` adds Household's take over (spec 3.3 §8.2) to the example.
if (params.has("takeOver")) client.hold(takeOverCard());
// `?detail` gives Code's release a feature's line under its details (spec 3.3 §7.1).
if (params.has("detail")) {
  client.hold({ ...releaseCard(), detail: { text: "From the invoice-run workflow", linkText: "Open in Runs", href: "/ui/runs" } });
}
// The phone's screen (spec 3.3 §5.4): `&phone=decide` its high-risk decision, `&phone=pair`
// its pairing, `&phone=long` a command cut short, `&phone=takeOver` a take over,
// `&phone=unreachable` a read that fails, else Decisions.
const phone = phoneFixture(params.get("phone"));
// `?screen=` shows another surface's screen over its own example: one line per surface.
const screens: Record<string, () => ReactNode> = {
  agents: () => agentsFixture(params, client),
  tray: () => <TrayPanel client={new TrayFixture()} />,
  phone: () => <PhoneApp client={phone.client} initial={phone.initial} />,
  voice: () => <Voice client={new VoiceFixtureClient()} devices={fixtureDevices()} onEnd={() => undefined} />,
  // `&fresh` starts at the setup code.
  website: () => <Website client={new WebsiteFixture({ fresh: params.has("fresh") })} onThreads={() => undefined} />,
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
