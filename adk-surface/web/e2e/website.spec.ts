// Spec 3.5 §8.2 end to end on `scripted_website`: a fresh bakery site is claimed with its setup
// code (A-I20) and onboarded through its nine stages to a signed release 1.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { shoot } from "./website-shared";

test.describe("a fresh site", () => {
  test("is claimed with its setup code and onboarded to a signed release", async ({ page }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/ui/website");

    // A-I20: nothing but the setup code until a device claims the site.
    await expect(page.getByRole("heading", { name: /Claim your site/ })).toBeVisible();
    await shoot(page, "01-setup-code");
    const code = readFileSync(join(process.env.WEBSITE_E2E_WORKSPACE!, "setup-code"), "utf8").trim();
    await page.getByLabel("Setup code").fill(code);
    await page.getByLabel("Name this device").fill("Amina's laptop");
    await page.getByRole("button", { name: "Claim this site" }).click();

    // 1 Start: from an online presence, with a phone for urgent alarms.
    await expect(page.getByRole("heading", { name: /Let's build your site/ })).toBeVisible();
    await page.getByRole("radio", { name: "I have profiles and listings" }).click();
    await page.getByLabel("Profiles and listings, one per line").fill("https://maps.example/place/juniper-bakery-nakuru");
    await page.getByLabel("A phone number for urgent alarms").fill("+254 700 123 456");
    await shoot(page, "02-start");
    await page.locator("form").getByRole("button", { name: "Start" }).click();

    // 2 What we found: every fact is a guess with its source until the owner confirms it.
    await expect(page.getByText("Each of these is a guess until you confirm it.", { exact: false })).toBeVisible();
    await expect(page.getByText("Opening hours: Mo-Sa 07:00-18:00")).toBeVisible();
    await expect(page.getByText("A guess").first()).toBeVisible();
    await shoot(page, "03-discovery");
    for (const fact of ["Opening hours", "Address", "Telephone", "Description"]) {
      const row = page.locator("li.fact", { hasText: fact });
      await row.getByRole("button", { name: "That's right" }).click();
      await expect(row.getByText("You said this")).toBeVisible();
    }
    await page.getByRole("button", { name: "Yes, that's it" }).click();

    // 4 A few questions: the live preview rebuilds after each answer.
    await expect(page.getByRole("heading", { name: "A few questions" })).toBeVisible();
    const about = page.locator("article", { hasText: "What is Juniper Bakery for" });
    await about.getByLabel("In your words").fill("Fresh rye and sourdough for Nakuru, baked every morning.");
    await about.getByRole("button", { name: "Save answer" }).click();
    const preview = page.getByRole("complementary", { name: "Your site so far" });
    await expect(preview.getByText("Fresh rye and sourdough for Nakuru, baked every morning.")).toBeVisible();
    const who = page.locator("article", { hasText: "Who is behind Juniper Bakery?" });
    await who.getByLabel("Names and roles you are happy to publish").fill("Amina Odhiambo, baker, and her brother Tom");
    await who.getByRole("button", { name: "Save answer" }).click();
    await expect(preview.getByText(/Amina Odhiambo/)).toBeVisible();
    const contact = page.locator("article", { hasText: "How can people reach you?" });
    await contact.getByLabel("Email").fill("hello@juniper.example");
    await contact.getByRole("button", { name: "Save answer" }).click();
    const alarms = page.locator("article", { hasText: "If something goes badly wrong" });
    await alarms.getByLabel("Urgent alarms").selectOption("sms");
    await alarms.getByLabel("Within the hour").selectOption("whatsapp");
    await alarms.getByRole("button", { name: "Save answer" }).click();
    await expect(page.locator("article", { hasText: "When are you open?" }).getByText("Answered from what we found")).toBeVisible();
    await shoot(page, "04-interview");
    await page.getByRole("button", { name: "Draft my charter" }).click();

    // 5 The charter: a card per section, read, one changed, then signed (A-I14, A-I23).
    await expect(page.getByRole("heading", { name: /Your charter · version 1/ })).toBeVisible();
    await expect(page.getByText("no target until it has a baseline").first()).toBeVisible();
    await shoot(page, "05-charter");
    const budget = page.locator("article", { has: page.getByRole("heading", { name: "Budget" }) });
    await budget.getByRole("button", { name: "Change it" }).click();
    await budget.getByLabel("Hosting (KES a month)").fill("1500");
    await budget.getByLabel("Models (KES a month)").fill("3000");
    await budget.getByLabel("Domains (KES a month)").fill("1500");
    await budget.getByRole("button", { name: "Save" }).click();
    await expect(budget.getByText("KES 1,500.00 a month").first()).toBeVisible();
    const sections = page.locator("article.w-section");
    for (let index = 0; index < (await sections.count()); index++) {
      const section = sections.nth(index);
      const looksRight = section.getByRole("button", { name: "Looks right" });
      if (await looksRight.count()) await looksRight.click();
      await expect(section.getByText("Read", { exact: true })).toBeVisible();
    }
    await page.getByRole("button", { name: "Sign as owner" }).click();
    await expect(page.getByText("Charter version 1 is signed and in force.")).toBeVisible();

    // 6 Design and build.
    await expect(page.getByRole("heading", { name: "Choose a look" })).toBeVisible();
    await shoot(page, "06-design");
    await page.locator("article", { hasText: "Harbour" }).getByRole("button", { name: "Choose this" }).click();
    await expect(page.getByText(/pages built from \d+ confirmed facts/)).toBeVisible();

    // 7 The launch check, page by page.
    await page.getByRole("button", { name: "Launch check" }).click();
    await page.getByRole("button", { name: "Run the check" }).click();
    await expect(page.getByText("Every check passed. Review each page, then connect and go live.")).toBeVisible({ timeout: 30_000 });
    await page.locator("li.w-page-row").first().getByRole("button", { name: "Show the page" }).click();
    await page.locator("li.w-page-row", { hasText: "/about/index.html" }).getByRole("button", { name: "Show the page" }).click();
    await expect(page.locator("li.w-page-row", { hasText: "/about/index.html" }).getByText(/Amina Odhiambo/)).toBeVisible();
    await shoot(page, "07-launch-check");

    // 8 Connect: the notice first; never a password (A-I19). Going live is the owner's (A-I21).
    await page.getByRole("button", { name: "Connect and go live" }).click();
    await expect(page.getByText(/I will never ask you for a password/)).toBeVisible();
    await page.locator("li.w-connection", { hasText: "Telegram" }).getByRole("button", { name: "Connect" }).click();
    await expect(page.getByText("JNPR-4821")).toBeVisible();
    await shoot(page, "08-connect");
    await page.getByRole("button", { name: "Go live", exact: true }).click();
    await expect(page.getByText("The site is live: release 1 is signed and published.")).toBeVisible({ timeout: 30_000 });

    // 9 Run: the 30/60/90-day plan.
    await expect(page.getByRole("heading", { name: "Your first 90 days" })).toBeVisible();
    await shoot(page, "09-run");
  });
});
