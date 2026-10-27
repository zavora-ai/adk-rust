// The website's views (spec 3.5 §8.2) over the fixture's bakery: the setup code before anything
// else (A-I20), onboarding's facts and answers, the owner's decisions on the site view
// (A-I13, A-I27), the weekly review and its summary, and the web editor (§7.8.2, N4).
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SETUP_CODE, WebsiteFixture } from "../src/fixture/website";
import { Editor } from "../src/website/Editor";
import { Onboarding } from "../src/website/Onboarding";
import { Review } from "../src/website/Review";
import { SiteView } from "../src/website/SiteView";
import { Website, websitePathOf, websiteViewOf } from "../src/website/Website";

afterEach(() => {
  window.history.replaceState(null, "", "/");
});

describe("the website's addresses", () => {
  it("names each view under /ui/website and nothing outside it", () => {
    expect(websiteViewOf("/ui/website")).toBeUndefined();
    expect(websiteViewOf("/ui/website/write")).toBe("write");
    expect(websiteViewOf("/ui/website/review")).toBe("review");
    expect(websiteViewOf("/ui/website/setup")).toBe("setup");
    expect(websiteViewOf("/ui/website/elsewhere")).toBe("site");
    expect(websiteViewOf("/ui/today")).toBeUndefined();
    expect(websitePathOf("site")).toBe("/ui/website");
    expect(websitePathOf("write")).toBe("/ui/website/write");
  });
});

describe("a fresh site", () => {
  it("takes nothing but its setup code, then opens onboarding", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture({ fresh: true });
    render(<Website client={client} onThreads={() => undefined} />);

    const claim = await screen.findByRole("button", { name: "Claim this site" });
    expect(claim).toHaveProperty("disabled", true);
    expect(screen.queryByRole("navigation", { name: "Website views" })).toBeNull();
    expect(screen.getByText("Juniper Bakery is opening soon.")).toBeTruthy();

    await user.type(screen.getByLabelText("Setup code"), "WRONG-CODE");
    await user.click(claim);
    expect((await screen.findByRole("alert")).textContent).toBe("That setup code is not right. Check the site's logs.");

    await user.clear(screen.getByLabelText("Setup code"));
    await user.type(screen.getByLabelText("Setup code"), ` ${SETUP_CODE} `);
    await user.clear(screen.getByLabelText("Name this device"));
    await user.type(screen.getByLabelText("Name this device"), "Amara's laptop");
    await user.click(claim);

    expect(client.taken.claims.at(-1)).toEqual({ code: SETUP_CODE, device: "Amara's laptop" });
    const tabs = await screen.findByRole("navigation", { name: "Website views" });
    expect(within(tabs).getByRole("link", { name: "Set up" }).getAttribute("aria-current")).toBe("true");
    expect(await screen.findByRole("navigation", { name: "Stages" })).toBeTruthy();
  });
});

describe("the website section", () => {
  it("opens a live site on its site view, and each tab is its own address", async () => {
    const user = userEvent.setup();
    render(<Website client={new WebsiteFixture()} onThreads={() => undefined} />);
    const tabs = await screen.findByRole("navigation", { name: "Website views" });
    expect(within(tabs).getByRole("link", { name: "Your site" }).getAttribute("aria-current")).toBe("true");
    expect(await screen.findByRole("heading", { name: "How it is doing" })).toBeTruthy();

    await user.click(within(tabs).getByRole("link", { name: "Weekly review" }));
    expect(window.location.pathname).toBe("/ui/website/review");
    expect(await screen.findByText("Week of 20 October", { selector: ".subhead" })).toBeTruthy();

    act(() => {
      window.history.pushState(null, "", "/ui/website/write");
      window.dispatchEvent(new PopStateEvent("popstate"));
    });
    expect(await screen.findByRole("button", { name: "New" })).toBeTruthy();
  });
});

describe("onboarding", () => {
  it("shows the nine stages with the one to do now, and confirms a found fact", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture({ fresh: true });
    client.boarding.current = "discover";
    client.boarding.stages = client.boarding.stages.map((stage) =>
      stage.id === "discover" ? { ...stage, state: "current" } : stage.number > 2 ? { ...stage, state: "later" } : stage,
    );
    render(<Onboarding client={client} onLaunched={() => undefined} />);

    const stages = await screen.findByRole("navigation", { name: "Stages" });
    expect(within(stages).getAllByRole("listitem")).toHaveLength(9);
    expect(within(stages).getByText("Discover").closest("li")?.getAttribute("aria-current")).toBe("step");
    // A stage not reached yet cannot be opened.
    expect(within(stages).queryByRole("button", { name: "Charter" })).toBeNull();

    const fact = screen.getByText("Opening hours: Tuesday to Sunday, 7:00 to 15:00").closest("li")!;
    expect(within(fact).getByText("A guess")).toBeTruthy();
    expect(within(fact).getByRole("link", { name: "maps.example" })).toBeTruthy();
    await user.click(within(fact).getByRole("button", { name: "That's right" }));

    expect(client.taken.onboarding).toEqual([{ type: "fact", id: "fact-hours", answer: "right" }]);
    const confirmed = (await screen.findByText("You said this")).closest("li")!;
    expect(within(confirmed).queryByRole("button", { name: "That's right" })).toBeNull();
  });

  it("fixes a found fact in the owner's own words", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture({ fresh: true });
    client.boarding.current = "discover";
    render(<Onboarding client={client} onLaunched={() => undefined} />);

    await user.click(await screen.findByRole("button", { name: "Fix it" }));
    const box = screen.getByDisplayValue("Opening hours: Tuesday to Sunday, 7:00 to 15:00");
    await user.clear(box);
    await user.type(box, "Opening hours: every day, 7:00 to 14:00");
    await user.click(screen.getByRole("button", { name: "Save" }));

    expect(client.taken.onboarding).toEqual([{ type: "fact", id: "fact-hours", answer: "fix", text: "Opening hours: every day, 7:00 to 14:00" }]);
    expect(await screen.findByText("Opening hours: every day, 7:00 to 14:00")).toBeTruthy();
  });

  it("saves an answer and shows the preview it rebuilt", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture({ fresh: true });
    render(<Onboarding client={client} onLaunched={() => undefined} />);

    expect(await screen.findByRole("heading", { name: "A few questions" })).toBeTruthy();
    const preview = screen.getByRole("complementary", { name: "Your site so far" });
    expect(within(preview).getByText(/1 things you told us/)).toBeTruthy();

    await user.type(screen.getByLabelText(/Public email/), "hello@juniper-bakery.example");
    await user.click(screen.getByRole("button", { name: "Save answer" }));

    expect(client.taken.onboarding).toEqual([{ type: "answer", form: "contact", values: { email: "hello@juniper-bakery.example" } }]);
    expect((await screen.findByRole("status")).textContent).toBe("The preview is rebuilt with your answer.");
    expect(within(preview).getByText(/2 things you told us/)).toBeTruthy();
    expect(screen.getByText("Answered")).toBeTruthy();
  });
});

describe("the site view", () => {
  it("shows status, the live release and what is held", async () => {
    render(<SiteView client={new WebsiteFixture()} />);
    expect(await screen.findByText("Up")).toBeTruthy();
    const release = screen.getByText("Live release").closest<HTMLElement>(".stat")!;
    expect(within(release).getByText("3")).toBeTruthy();
    // One held item and one held delivery.
    const held = screen.getAllByText("Held for you")[0].closest<HTMLElement>(".stat")!;
    expect(within(held).getByText("2")).toBeTruthy();
    expect(screen.getByRole("row", { name: /Orders from the site/ })).toBeTruthy();
  });

  it("promotes a job by the owner's decision and says what changed", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    render(<SiteView client={client} />);

    await user.click(await screen.findByRole("button", { name: "Let it do more: Act and tell" }));
    expect(client.taken.site).toEqual([{ type: "promote", job: "menu" }]);
    expect((await screen.findByRole("status")).textContent).toBe("Keep the menu current now does more: Act and tell.");
    expect(screen.queryByRole("button", { name: /Let it do more/ })).toBeNull();
  });

  it("lowers a job only with a reason", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    render(<SiteView client={client} />);

    const row = (await screen.findByText("Keep opening hours right")).closest("li")!;
    await user.click(within(row).getByRole("button", { name: "Lower" }));
    await user.type(within(row).getByLabelText("Why"), "It changed the Sunday hours");
    await user.click(within(row).getByRole("button", { name: "Lower" }));

    expect(client.taken.site).toEqual([{ type: "lower", job: "hours", reason: "It changed the Sunday hours" }]);
  });

  it("lists six jobs, then every one on request", async () => {
    const user = userEvent.setup();
    render(<SiteView client={new WebsiteFixture()} />);
    const jobs = (await screen.findByRole("heading", { name: "What your site does" })).closest("section")!;
    expect(within(jobs).getAllByRole("listitem")).toHaveLength(7);
    await user.click(within(jobs).getByRole("button", { name: "Show all 8 jobs" }));
    expect(within(jobs).getAllByRole("listitem")).toHaveLength(8);
    expect(within(jobs).getByRole("button", { name: "Show fewer" })).toBeTruthy();
  });

  it("publishes a held item, discards a held delivery whole and acknowledges an alarm", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    render(<SiteView client={client} />);

    await user.click(await screen.findByRole("button", { name: "Publish it" }));
    await waitFor(() => expect(screen.queryByText("Pumpkin loaf")).toBeNull());
    await user.click(screen.getByRole("button", { name: "Discard it whole" }));
    await waitFor(() => expect(screen.queryByText(/Supplier stock/)).toBeNull());
    expect(screen.getByText("Nothing is held.")).toBeTruthy();

    const alarms = screen.getByRole("region", { name: "Alarms" });
    await user.click(within(alarms).getByRole("button", { name: "Acknowledge" }));
    await waitFor(() => expect(screen.queryByRole("region", { name: "Alarms" })).toBeNull());

    expect(client.taken.site).toEqual([
      { type: "item", id: "held-1", decision: "release" },
      { type: "delivery", delivery: 7, decision: "discard" },
      { type: "acknowledge", alarm: "alarm-1" },
    ]);
  });

  it("opens a release to the pages it changed", async () => {
    const user = userEvent.setup();
    render(<SiteView client={new WebsiteFixture()} />);
    const toggle = await screen.findByRole("button", { name: "2 pages changed" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    await user.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("/menu/autumn")).toBeTruthy();
    expect(screen.getByText("and 2 other files")).toBeTruthy();
  });
});

describe("the weekly review", () => {
  it("sends its summary to the owner's channel once", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    render(<Review client={client} onPromote={async () => undefined} onThreads={() => undefined} />);

    expect(await screen.findByText(/14 orders, up from 9/)).toBeTruthy();
    const updates = screen.getByRole("heading", { name: "Updates from your feeds" }).closest("section")!;
    expect(within(updates).getByText("46 (40 live, 6 in releases)")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Send the summary to WhatsApp" }));
    expect(client.taken.review).toEqual([{ type: "send", week: "2026-W43" }]);
    expect(await screen.findByText("Sent to WhatsApp just now")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Send the summary/ })).toBeNull();
  });

  it("accepts an earned promotion as the owner's decision and sends questions to Threads", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    const onThreads = vi.fn();
    render(<Review client={client} onPromote={async (job) => void (await client.decideSite({ type: "promote", job }))} onThreads={onThreads} />);

    await user.click(await screen.findByRole("button", { name: "Let it do more: Act and tell" }));
    expect(client.taken.site).toEqual([{ type: "promote", job: "menu" }]);
    await waitFor(() => expect(screen.queryByRole("button", { name: /Let it do more/ })).toBeNull());

    await user.click(screen.getByRole("button", { name: "Answer in Threads" }));
    expect(onThreads).toHaveBeenCalledOnce();
  });

  it("reads as a document, and opens an earlier week", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    render(<Review client={client} onPromote={async () => undefined} onThreads={() => undefined} />);

    await user.click(await screen.findByRole("button", { name: "As a document" }));
    expect(screen.getByRole("heading", { level: 1, name: "Week of 20 October" })).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Week of 13 October" }));
    expect(client.taken.reviewWeeks).toEqual([undefined, "2026-W42"]);
    expect(await screen.findByText("Week of 13 October", { selector: ".subhead" })).toBeTruthy();
  });
});

describe("the web editor", () => {
  it("drafts a post from its form, previews every page it touches and publishes it", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    render(<Editor client={client} />);

    await user.click(await screen.findByRole("button", { name: "New" }));
    expect(client.taken.editor).toEqual([{ type: "new", kind: "post" }]);
    expect(await screen.findByRole("heading", { name: "Post" })).toBeTruthy();

    await user.type(screen.getByLabelText("Address"), "pumpkin-week");
    await user.type(screen.getByLabelText(/^Title/), "Pumpkin week");
    await user.type(screen.getByLabelText("Text"), "Pumpkin loaves all week.");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(client.taken.editor.at(-1)).toEqual({
      type: "save",
      draft: "draft-1",
      slug: "pumpkin-week",
      language: "en",
      fields: { title: "Pumpkin week", summary: "" },
      body: "Pumpkin loaves all week.",
      schedule: {},
    });

    await user.click(await screen.findByRole("button", { name: "Preview the changes" }));
    const changes = await screen.findByRole("region", { name: "What changes" });
    expect(within(changes).getByText("/posts/pumpkin-week")).toBeTruthy();
    expect(screen.getByText("Every check passed.")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Publish" }));
    expect(await screen.findByText("Published in release 4.")).toBeTruthy();
    // A published draft is read only.
    expect(screen.queryByRole("button", { name: "Publish" })).toBeNull();
    expect(screen.getByLabelText("Address").closest("fieldset")).toHaveProperty("disabled", true);
  });

  it("wraps the selected text from the formatting buttons", async () => {
    const user = userEvent.setup();
    render(<Editor client={new WebsiteFixture()} />);
    await user.click(await screen.findByRole("button", { name: "New" }));

    const text = (await screen.findByLabelText("Text")) as HTMLTextAreaElement;
    await user.type(text, "fresh bread");
    text.setSelectionRange(6, 11);
    await user.click(screen.getByRole("button", { name: "Bold" }));
    expect(text.value).toBe("fresh **bread**");
  });

  it("corrects live content only with a note readers see", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    render(<Editor client={client} />);

    const item = (await screen.findByText("Our autumn menu")).closest("li")!;
    await user.click(within(item).getByRole("button", { name: "Correct" }));
    const submit = within(item).getByRole("button", { name: "Correct" });
    expect(submit).toHaveProperty("disabled", true);
    await user.type(within(item).getByLabelText("Correction note readers see"), "The price of the rye loaf was wrong.");
    await user.click(submit);

    expect(client.taken.editor).toEqual([{ type: "correct", id: "item-1", note: "The price of the rye loaf was wrong." }]);
    expect((await screen.findByRole("status")).textContent).toBe("Corrected: readers see your note.");
  });

  it("shows who must approve a release and refuses one with a reason", async () => {
    const user = userEvent.setup();
    const client = new WebsiteFixture();
    await client.edit({ type: "new", kind: "post" });
    const draft = client.drafts.get("draft-1")!;
    draft.status = { text: "Waiting for approval", tone: "waiting" };
    draft.approval = {
      text: "This release needs the publisher's approval.",
      proposer: "Amara",
      canApprove: ["publisher"],
      lines: [{ role: "publisher", label: "Publisher", holders: ["Kofi"] }],
    };
    render(<Editor client={client} />);

    await user.click(await screen.findByRole("button", { name: /Untitled/ }));
    const approvals = await screen.findByRole("region", { name: "Approvals" });
    expect(within(approvals).getByText("Proposed by Amara, who cannot approve it.")).toBeTruthy();
    expect(within(approvals).getByText("Waiting")).toBeTruthy();
    expect(within(approvals).getByRole("button", { name: "Approve as publisher" })).toBeTruthy();

    await user.click(within(approvals).getByRole("button", { name: "Refuse" }));
    await user.type(within(approvals).getByLabelText("Why you refuse it"), "Wrong photo");
    await user.click(within(approvals).getAllByRole("button", { name: "Refuse" }).at(-1)!);
    expect(client.taken.editor.at(-1)).toEqual({ type: "reject", draft: "draft-1", reason: "Wrong photo" });
  });
});
