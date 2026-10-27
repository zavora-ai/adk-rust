// Spec 3.3 §6.1 `genui-apps`: apps from MCP servers over the web home's example
// (`e2e/fixture-server.sh`), in light and dark. Weather's app is a real page framed sandboxed,
// speaking the MCP Apps handshake to the page's bridge; it cannot reach the home's document,
// sits on the card background with no decision colours inside, and what it asks for shows
// outside its frame. `GENUI_APPS_E2E_SHOTS` names a folder for the screenshots.
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const shots = process.env.GENUI_APPS_E2E_SHOTS;

/** The class names of everything inside an app's container that carries a decision's colours or a pill. */
async function decisionInsideApps(page: Page): Promise<string[]> {
  return page.locator(".drawn-app").evaluateAll((apps) =>
    apps.flatMap((app) => [...app.querySelectorAll("*")].map((element) => element.className.toString()).filter((name) => /decision|pill/.test(name))),
  );
}

test("an app in Chief of staff's thread, light and dark", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 980 });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/ui/fixture.html?screen=genui&genui=app");
    await page.getByRole("button", { name: /^Chief of staff/ }).click();
    const app = page.locator(".drawn-app");
    await expect(app.getByText("App from Weather · it can't approve anything")).toBeVisible();
    // The app's own page drew the tool's result it was sent through the bridge.
    const weather = page.frameLocator(".app-frame");
    await expect(weather.locator("#city")).toHaveText("Nairobi");
    expect(await page.locator(".app-frame").getAttribute("sandbox")).toBe("allow-scripts");
    // A sandboxed frame has no origin of its own: it can't read or act on the home's page.
    const frame = page.frames().find((candidate) => candidate.url().includes("weather.html"));
    expect(await frame?.evaluate(() => {
      try {
        return window.parent.document.title;
      } catch {
        return "blocked";
      }
    })).toBe("blocked");
    expect(await decisionInsideApps(page)).toEqual([]);
    if (shots) {
      mkdirSync(shots, { recursive: true });
      await page.screenshot({ path: join(shots, `genui-app-${scheme}.png`) });
    }
  }
});

test("every state of an app, light and dark", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 2200 });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/ui/fixture.html?screen=genui&genui=app-states");
    await expect(page.getByText("Opening the app…")).toBeVisible();
    await expect(page.getByText("This app was blocked: it shows approval buttons")).toBeVisible();
    await expect(page.getByText("Open the home on this computer to see this app")).toBeVisible();
    // What the apps asked for, relayed by the bridge and answered by the home.
    await expect(page.getByText("The app tried to use transfer_funds. Apps can't use it.")).toBeVisible();
    const message = page.getByRole("group", { name: "The app wants to send: “Remind me to take an umbrella tomorrow.”" });
    await expect(message.getByRole("button", { name: "Send it" })).toBeVisible();
    await expect(page.getByRole("group", { name: "Open example.com?" }).getByRole("button", { name: "Open" })).toBeVisible();
    await expect(page.getByText("The app from Weather asked · 10:06")).toBeVisible();
    // Every shown app drew its forecast.
    for (const frame of await page.locator(".app-frame").all()) {
      await expect(frame.contentFrame().locator("#city")).toHaveText("Nairobi");
    }
    // Nothing an app asked for is inside its container, and no app sits on a decision.
    expect(await decisionInsideApps(page)).toEqual([]);
    expect(await page.locator(".decision .drawn-app, .drawn-app .app-ask, .drawn-app .decision").count()).toBe(0);
    if (shots) {
      mkdirSync(shots, { recursive: true });
      await page.screenshot({ path: join(shots, `genui-app-states-${scheme}.png`), fullPage: true });
    }
  }
});
