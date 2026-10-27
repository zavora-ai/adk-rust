// Spec 3.3 §12 "Governed form" end to end over `examples/scripted_genui.rs`: Finance draws the
// invoice form and a screen that looks like a decision; the form shows on the card background
// with no decision colours or pills, in light and dark; the screen is a plain notice; the
// person sends the form and its call waits under it, never offering Always allow; Allow once
// runs it exactly once. `GENUI_E2E_SHOTS` names a folder for light and dark screenshots.
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const workspace = process.env.GENUI_E2E_WORKSPACE ?? "";
const shots = process.env.GENUI_E2E_SHOTS;

async function shoot(page: Page, name: string) {
  if (!shots) return;
  mkdirSync(shots, { recursive: true });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.screenshot({ path: join(shots, `${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: "light" });
}

/** No decision background, decision class or pill inside a drawing, in light and dark. */
async function expectNoDecisionInside(page: Page) {
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    const inside = await page.locator(".drawn").evaluateAll((drawings) =>
      drawings.flatMap((drawing) =>
        [drawing, ...drawing.querySelectorAll("*")]
          .filter((element) => /decision|pill/.test(element.className.toString()))
          .map((element) => element.className.toString()),
      ),
    );
    expect(inside, scheme).toEqual([]);
    const background = await page.locator(".drawn").first().evaluate((element) => getComputedStyle(element).backgroundColor);
    const card = await page.evaluate(() => {
      const probe = document.createElement("div");
      probe.style.background = "var(--card)";
      document.body.append(probe);
      const color = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return color;
    });
    expect(background, scheme).toBe(card);
  }
  await page.emulateMedia({ colorScheme: "light" });
}

test("a drawn form is sent, waits for the person, and runs once on Allow once", async ({ page }) => {
  await page.goto("/ui/");
  const box = page.getByRole("textbox", { name: "Message your agents" });
  await box.fill("Prepare the October invoice for ACME");
  await box.press("Enter");

  const main = page.getByRole("main");
  const form = main.locator(".drawn");
  await expect(form).toHaveCount(1, { timeout: 20_000 });
  await expect(form.getByText("Drawn by Finance")).toBeVisible();
  await expect(form.getByText("Sending runs send_invoice and asks you first")).toBeVisible();
  await expect(main.getByText("Finance tried to show something that looks like a decision. It wasn't shown.")).toBeVisible();
  await expect(main.getByText(/catalog forms 1.0/)).toHaveCount(0);
  await expect(main.getByText("Here is the invoice. Check the details and press Send when it is right.")).toBeVisible();
  await expectNoDecisionInside(page);
  await shoot(page, "genui-form");

  await form.getByLabel("To", { exact: true }).fill("ana@example.com");
  await form.getByRole("button", { name: "Send" }).click();

  // The call waits under the form, as Finance, asked for by the person.
  const card = main.getByRole("article", { name: /send_invoice|Send invoice/ });
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card.getByText(/You pressed Send on a form Finance drew/)).toBeVisible();
  await expect(card.getByText("From a form Finance drew")).toBeVisible();
  await expect(card.getByRole("button", { name: /Always allow/ })).toHaveCount(0);
  await expect(form.getByText(/waiting for you/)).toBeVisible();
  await expect(form.getByRole("button", { name: "Send" })).toHaveCount(0);
  expect(existsSync(join(workspace, "sent.txt"))).toBe(false);
  await expectNoDecisionInside(page);
  await shoot(page, "genui-card");

  await card.getByRole("button", { name: "Allow once" }).click();
  await expect(form.getByText(/Sent · done at/)).toBeVisible({ timeout: 20_000 });
  const sent = readFileSync(join(workspace, "sent.txt"), "utf8").trim().split("\n");
  expect(sent).toHaveLength(1);
  expect(JSON.parse(sent[0])).toEqual({ to: "ana@example.com", amount: 4200, invoice: "INV-10" });
  await shoot(page, "genui-done");
});
