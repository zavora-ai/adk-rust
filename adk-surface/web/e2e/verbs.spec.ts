// Spec 3.3 §2 item 7, §12 "One verb, three origins" end to end: Finance asks to snooze News
// digest and the card waits for the person, whatever its mode; Allow once moves it. Then the
// person snoozes it again from Coming up with a real pointer, and the row says so only once
// the server answered.
import { expect, test } from "@playwright/test";
import type { APIRequestContext } from "@playwright/test";

/** When News digest wakes, as the kernel's agenda says. */
async function wake(request: APIRequestContext): Promise<number> {
  const agenda = await (await request.get("/api/kernel/agenda?state=scheduled")).json();
  const item = agenda.items.find((candidate: { title: string }) => candidate.title === "News digest");
  return Date.parse(item.state.scheduled.wake.anyOf[0].at);
}

const HOUR = 3_600_000;

test("an agent's request to snooze waits for the person, and Allow once moves it", async ({ page, request }) => {
  const before = await wake(request);
  await page.goto("/ui/");
  const box = page.getByRole("textbox", { name: /Message/ });
  await box.fill("Tidy my evening");
  await box.press("Enter");

  const card = page.getByRole("main").getByRole("article", { name: /^Move News digest to / });
  await expect(card).toBeVisible({ timeout: 30_000 });
  await expect(card.getByRole("button", { name: "Allow once" })).toBeVisible();
  await expect(card.getByRole("button", { name: "Edit" })).toBeVisible();
  await expect(card.getByRole("button", { name: "Deny" })).toBeVisible();
  await expect(card.getByRole("button", { name: /Always allow/ })).toHaveCount(0);
  await expect(card.getByText("You can change it back")).toBeVisible();
  await expect(card.getByText("Agents always ask before they change things on your screens.")).toBeVisible();
  expect(await wake(request)).toBe(before);

  await card.getByRole("button", { name: "Allow once" }).click();
  await expect.poll(() => wake(request), { timeout: 20_000 }).toBe(before + HOUR);
});

test("Snooze on Coming up, with a real pointer", async ({ page, request }) => {
  const before = await wake(request);
  await page.goto("/ui/");
  const comingUp = page.getByRole("region", { name: "Coming up" });
  const row = comingUp.getByRole("listitem").filter({ hasText: "News digest" });
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.hover();
  await row.getByRole("button", { name: "Snooze News digest" }).click();
  const menu = comingUp.getByRole("menu");
  await expect(menu.getByRole("menuitem")).toHaveCount(2);
  await menu.getByRole("menuitem", { name: /1 hour later/ }).click();
  await expect(comingUp.getByText("Snoozed by you · just now")).toBeVisible();
  await expect.poll(() => wake(request), { timeout: 20_000 }).toBe(before + HOUR);

  // "On your screens" in Memory & rules.
  await page.goto("/ui/memory");
  await expect(page.getByText("On your screens", { exact: true })).toBeVisible();
  await expect(page.getByText("Snooze something in Coming up")).toBeVisible();
});
