// Spec 3.3 §5.4, §8.2, §8.3 end to end: the home adds a phone; the phone pairs inside the
// app's scope, sets up its passkey and confirms a high-risk call the scripted agent's harness
// holds; then the home sets up the person's passkey on this computer and confirms the next one
// itself. Each page has its own virtual platform authenticator that verifies the user, as Face
// ID or Touch ID does. `PHONE_E2E_SHOTS` names a folder for light and dark screenshots.
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const workspace = process.env.PHONE_E2E_WORKSPACE ?? "";
const shots = process.env.PHONE_E2E_SHOTS;

/** A platform authenticator for `page` that verifies the user. */
async function authenticator(page: Page) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
}

/** `page` as it looks in light and in dark, when screenshots are asked for. */
async function shoot(page: Page, name: string) {
  if (!shots) return;
  mkdirSync(shots, { recursive: true });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.screenshot({ path: join(shots, `${name}-${scheme}.png`), fullPage: true });
  }
  await page.emulateMedia({ colorScheme: "light" });
}

/** Sends `text` from the home's composer: "Message your agents", then "Message <agent>". */
async function ask(home: Page, text: string) {
  await home.goto("/ui/");
  const box = home.getByRole("textbox", { name: /^Message / });
  await box.fill(text);
  await box.press("Enter");
}

test("a phone pairs and confirms a high-risk call with its passkey; the home confirms with its own", async ({ browser, baseURL }) => {
  const home = await (await browser.newContext({ baseURL })).newPage();
  await authenticator(home);

  // "Add a phone or app": the QR code, the link inside the app's scope and the code.
  await home.goto("/ui/memory");
  await home.getByRole("button", { name: "Add a phone or app" }).click();
  await expect(home.getByRole("img", { name: /QR code of the pairing link/ })).toBeVisible();
  const link = (await home.locator(".pairing-link").textContent()) ?? "";
  expect(link).toMatch(new RegExp(`^${baseURL}/ui/pair#code=`));

  // The phone opens the link, pairs, and sets up its passkey; notifications stay off here.
  const phone = await (await browser.newContext({ baseURL, viewport: { width: 390, height: 844 }, hasTouch: true })).newPage();
  await authenticator(phone);
  await phone.goto(link);
  await phone.getByRole("button", { name: "Pair this phone" }).click();
  await phone.getByRole("button", { name: "Allow notifications" }).click();
  await phone.getByRole("button", { name: "Use your face or fingerprint", exact: true }).click();
  await expect(phone.getByText("This phone asks for your face or fingerprint.")).toBeVisible();
  await phone.getByRole("button", { name: "Go to Decisions" }).click();
  await expect(home.getByText("asks for your face or fingerprint")).toBeVisible({ timeout: 15_000 });

  // The harness holds a high-risk write; the phone's card says it needs the person.
  await ask(home, "Write the release notes");
  const card = home.getByRole("main").getByRole("article", { name: "Write file release.txt" });
  await expect(card).toBeVisible({ timeout: 20_000 });
  const waiting = phone.getByRole("list", { name: "Decisions" }).getByRole("button", { name: /release\.txt/ });
  await expect(waiting).toBeVisible({ timeout: 20_000 });
  await expect(waiting).toContainText("needs your face or fingerprint");
  await shoot(phone, "phone-decisions");
  await waiting.click();
  const confirm = phone.getByRole("button", { name: /Confirm it's you and approve/ });
  await expect(confirm).toBeVisible();
  await shoot(phone, "phone-decide");
  await confirm.click();

  // Confirmed on the phone, the parked process wakes and the write runs once.
  await expect(phone.getByText(/^You said yes on your phone at \d\d:\d\d · done once$/)).toBeVisible({ timeout: 20_000 });
  await expect(home.getByRole("main").getByText("The release notes are written.")).toBeVisible({ timeout: 20_000 });
  expect(readFileSync(join(workspace, "release.txt"), "utf8")).toBe("v3.0.0");

  // The home: the person's passkey on this computer, then a high-risk answer confirmed with it.
  await home.goto("/ui/memory");
  await home.getByRole("button", { name: "Use your face or fingerprint on this computer" }).click();
  await expect(home.getByRole("listitem").filter({ hasText: /^Face or fingerprint on this computer · / })).toBeVisible();
  await expect(home.getByRole("button", { name: "Use your face or fingerprint on this computer" })).toHaveCount(0);
  await ask(home, "Write the notes");
  const notes = home.getByRole("main").getByRole("article", { name: "Write file notes.txt" });
  await expect(notes).toBeVisible({ timeout: 20_000 });
  expect(existsSync(join(workspace, "notes.txt"))).toBe(false);
  await notes.getByRole("button", { name: /Confirm it's you and approve/ }).click();
  await expect(home.getByRole("main").getByText("The notes are written.")).toBeVisible({ timeout: 20_000 });
  expect(readFileSync(join(workspace, "notes.txt"), "utf8")).toBe("Notes");
});
