// Every screen of the web home's example passes axe at WCAG 2.1 AA in light and dark (spec 3.3
// §9): contrast, names, roles and structure, in a real browser so colour contrast is measured.
import { createRequire } from "node:module";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const require = createRequire(import.meta.url);
const AXE = require.resolve("axe-core/axe.min.js");

type Violation = { id: string; impact: string | null; help: string; nodes: Array<{ target: string[] }> };

async function violations(page: Page): Promise<Violation[]> {
  await page.addScriptTag({ path: AXE });
  return page.evaluate(async () => {
    const axe = (window as unknown as { axe: { run: (context: Document, options: object) => Promise<{ violations: Violation[] }> } }).axe;
    const result = await axe.run(document, {
      runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"] },
      resultTypes: ["violations"],
    });
    return result.violations.map(({ id, impact, help, nodes }) => ({ id, impact, help, nodes: nodes.map(({ target }) => ({ target })) }));
  });
}

/** The serious and critical ones, said plainly enough to fix. */
function serious(found: Violation[]): string[] {
  return found
    .filter((violation) => violation.impact === "serious" || violation.impact === "critical")
    .map((violation) => `${violation.id} (${violation.impact}): ${violation.help} — ${violation.nodes.slice(0, 3).map((node) => node.target.join(" ")).join(", ")}`);
}

const SCREENS: Array<{ name: string; query: string; size?: { width: number; height: number }; then?: (page: Page) => Promise<void> }> = [
  { name: "threads", query: "section=threads" },
  {
    name: "code thread with an image and runs",
    query: "section=threads",
    then: async (page) => {
      await page.getByRole("navigation", { name: "Agents" }).getByRole("button", { name: /Code/ }).click();
      await page.getByText("cargo test -p parser").first().waitFor();
    },
  },
  {
    name: "command palette",
    query: "section=today",
    then: async (page) => {
      await page.keyboard.press("ControlOrMeta+k");
      await page.getByRole("dialog", { name: "Search and jump" }).waitFor();
    },
  },
  { name: "today", query: "section=today" },
  { name: "memory and rules", query: "section=memory" },
  { name: "developer run view", query: "screen=developer", then: async (page) => void (await page.getByRole("tree").waitFor()) },
  { name: "voice", query: "screen=voice" },
  { name: "phone", query: "screen=phone", size: { width: 390, height: 844 } },
  { name: "tray", query: "screen=tray", size: { width: 420, height: 700 } },
];

for (const scheme of ["light", "dark"] as const) {
  test.describe(`${scheme}`, () => {
    test.use({ colorScheme: scheme });
    for (const screen of SCREENS) {
      test(`${screen.name} has no serious accessibility problems`, async ({ page }) => {
        if (screen.size) await page.setViewportSize(screen.size);
        await page.emulateMedia({ reducedMotion: "reduce" });
        await page.goto(`/ui/fixture.html?${screen.query}`);
        await page.locator("#root > *").first().waitFor();
        await screen.then?.(page);
        await page.waitForTimeout(300);
        expect(serious(await violations(page))).toEqual([]);
      });
    }
  });
}
