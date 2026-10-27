// Spec 3.3 §6.1 `genui`: every state of the agent-drawn UI concepts over the web home's example
// (`e2e/fixture-server.sh`), in light and dark, with no decision colours or pills inside a
// drawing. `GENUI_STATES_E2E_SHOTS` names a folder for the screenshots.
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";

const shots = process.env.GENUI_STATES_E2E_SHOTS;

const PAGES: Array<{ name: string; query: string; height: number; tab?: string }> = [
  { name: "genui-form", query: "genui=form", height: 980 },
  { name: "genui-form-states", query: "genui=states", height: 2300 },
  { name: "genui-card", query: "genui=sent", height: 980 },
  { name: "genui-card-states", query: "genui=card-states", height: 1500 },
  { name: "genui-display", query: "genui=display", height: 980 },
  { name: "genui-display-states", query: "genui=display-states", height: 1300 },
  { name: "genui-refused", query: "genui=refused", height: 980 },
  { name: "genui-refused-states", query: "genui=refused-states", height: 400 },
  { name: "genui-protocols", query: "genui=protocols", height: 1300, tab: "Protocols" },
  { name: "genui-phone", query: "genui=form", height: 844 },
];

for (const shot of PAGES) {
  test(`${shot.name} in light and dark`, async ({ page }) => {
    const width = shot.name === "genui-phone" ? 390 : 1440;
    await page.setViewportSize({ width, height: shot.height });
    for (const scheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.goto(`/ui/fixture.html?screen=genui&${shot.query}`);
      if (shot.tab) await page.getByRole("button", { name: shot.tab }).click();
      if (!shot.query.includes("protocols") && !shot.query.includes("refused-states")) {
        await expect(page.locator(".drawn").first()).toBeVisible();
      }
      const inside = await page.locator(".drawn").evaluateAll((drawings) =>
        drawings.flatMap((drawing) =>
          [...drawing.querySelectorAll("*")].map((element) => element.className.toString()).filter((name) => /decision|pill/.test(name)),
        ),
      );
      expect(inside).toEqual([]);
      if (shots) {
        mkdirSync(shots, { recursive: true });
        await page.screenshot({ path: join(shots, `${shot.name}-${scheme}.png`) });
      }
    }
  });
}
