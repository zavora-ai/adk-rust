// Spec 3.3 §5.1, §10 K3, K4, §12 phase 2 end to end, on the scripted kernel with its `/proc`
// ledger: after a turn with one decision allowed from the home, the morning briefing shows on
// Threads once quiet time has ended; Today lists what happened in plain words, each line
// opening its record; opening the page moves nothing, and only Mark as seen does; and a
// changed quiet time shows its old and new value.
import { expect, test } from "@playwright/test";

test("the morning briefing and Today read the kernel's page, and only Mark as seen moves on", async ({ page, request }) => {
  await page.goto("/ui/");
  await expect(page.getByRole("button", { name: "Stop all" })).toBeVisible();

  // A turn with one decision, allowed once from the home. The scripted agent answers each
  // turn with "… written."; an earlier test's turn may already be in the thread.
  const written = page.getByRole("main").getByText(/ written\.$/);
  await page.waitForTimeout(1_000);
  const before = await written.count();
  const box = page.getByRole("main").getByRole("textbox");
  await box.fill("Write the notes");
  await box.press("Enter");
  const card = page.getByRole("main").getByRole("article").last();
  await expect(card.getByRole("button", { name: "Allow once" })).toBeVisible({ timeout: 20_000 });
  await card.getByRole("button", { name: "Allow once" }).click();
  await expect(written).toHaveCount(before + 1, { timeout: 20_000 });

  // Quiet time ended an hour ago: the briefing shows on Threads, from the kernel's page.
  const briefing = page.getByRole("region", { name: "While you were away" });
  await expect(briefing).toBeVisible({ timeout: 20_000 });
  await expect(briefing.getByText("Quiet time respected")).toBeVisible();
  await expect(briefing.getByText(/ finished/)).toBeVisible();
  await briefing.getByRole("button", { name: "See what happened" }).click();

  // Today: one line per event, in plain words, never a raw id or the kernel's housekeeping.
  await expect(page.getByRole("heading", { name: /While you were away/ })).toBeVisible();
  const list = page.locator("ol.briefing");
  await expect(list.getByText("Finance finished.").first()).toBeVisible({ timeout: 20_000 });
  const texts = await list.locator(".briefing-text").allTextContents();
  expect(texts.length).toBeGreaterThan(0);
  expect(texts.length).toBeLessThanOrEqual(6);
  for (const text of texts) expect(text).not.toMatch(/urn:|Urn:|Kernel|changed what it is doing|woke up/);

  // Every line opens its record, read through the page's client and told in plain words.
  const rows = list.locator("li.briefing-row");
  for (let index = 0; index < (await rows.count()); index++) {
    const row = rows.nth(index);
    await row.getByRole("button", { name: "Details" }).click();
    const entry = row.getByText(/^On record as entry \d+$/);
    await expect(entry).toBeVisible();
    await expect(row.getByText(/^Recorded /)).toBeVisible();
    await expect(row.getByRole("link")).toHaveCount(0);
    const seq = (await entry.textContent())!.split(" ").pop();
    const record = await (await request.get(`/api/kernel/away/lines/${seq}`)).json();
    expect(String(record.entry.seq)).toBe(seq);
  }

  // Opening the page again moves nothing.
  await page.reload();
  await expect(list.getByText("Finance finished.").first()).toBeVisible({ timeout: 20_000 });

  // Mark as seen: the lines are gone, after a reload too, and the briefing does not come back.
  await page.getByRole("button", { name: "Mark as seen" }).click();
  await expect(page.getByText("Marked as seen. New lines will show here.")).toBeVisible();
  await expect(list.getByText("Finance finished.")).toHaveCount(0);
  await page.reload();
  await expect(page.getByText(/^Nothing has happened since /)).toBeVisible({ timeout: 20_000 });
  await page.getByRole("link", { name: /Threads/ }).click();
  await expect(page.getByRole("heading", { name: /^Decisions/ })).toBeVisible();
  await expect(page.getByRole("region", { name: "While you were away" })).toHaveCount(0);

  // What your agents can do: a changed quiet time shows its old and new value.
  await page.getByRole("link", { name: "Memory & rules" }).click();
  const canDo = page.locator("section", { has: page.getByRole("heading", { name: "What your agents can do" }) });
  await canDo.getByRole("button", { name: "Change" }).first().click();
  await canDo.getByLabel("From").fill("23:00");
  await canDo.getByLabel("Until").fill("06:00");
  await canDo.getByRole("button", { name: "Save" }).click();
  await expect(canDo.getByText(/^Was \d\d:\d\d–\d\d:\d\d\. Now 23:00–06:00\.$/)).toBeVisible();
  const settings = await (await request.get("/api/kernel/settings")).json();
  // Saved with the browser's zone and offset, so the kernel keeps the hours typed (spec 3.3
  // §5.1); a kernel that reads offsets only keeps the offset.
  const [window] = settings.areas[0].quietTime;
  expect(window).toMatchObject({
    start: "23:00",
    end: "06:00",
    // At UTC the browser's offset reads -0, which JSON carries as 0.
    utcOffsetMinutes: (await page.evaluate(() => -new Date().getTimezoneOffset())) || 0,
  });
  if (window.timeZone !== undefined) {
    expect(window.timeZone).toBe(await page.evaluate(() => Intl.DateTimeFormat().resolvedOptions().timeZone));
  }
});
