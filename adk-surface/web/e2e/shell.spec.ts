// Spec 3.3 §5.1, §5.4, §5.5 end to end over the web home's example (`e2e/fixture-server.sh`):
// a fixed app shell. At every size, and after every resize, nothing scrolls as a page and
// nothing shows past the app; the header stays put; the rail, the thread and the right panel
// scroll inside themselves with the message box in view; Today and Memory & rules scroll inside
// their own area; under the phone width the column scrolls under the header; the tray panel
// and the phone fill their window with their own buttons pinned. `SHELL_E2E_SHOTS` names a
// folder for light and dark screenshots.
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

const shots = process.env.SHELL_E2E_SHOTS;

/** Desktop, between the breakpoints, and the phone, each short and tall. */
const SIZES = [
  { width: 1440, height: 900 },
  { width: 1280, height: 600 },
  { width: 1024, height: 560 },
  { width: 800, height: 900 },
  { width: 390, height: 844 },
  { width: 354, height: 807 },
  { width: 354, height: 480 },
];

/**
 * The window shows `app` and nothing else: the document is no taller than the window, the app
 * fills it, its last row of pixels is the app's, and the page cannot be scrolled or bounced.
 */
async function expectShell(page: Page, app: string) {
  await page.mouse.move(10, 10);
  await page.mouse.wheel(0, 4_000);
  const shell = await page.evaluate((selector) => {
    const root = document.documentElement;
    const box = document.querySelector(selector)?.getBoundingClientRect();
    const corner = document.elementFromPoint(innerWidth - 2, innerHeight - 1);
    return {
      scrollHeight: root.scrollHeight,
      bodyHeight: document.body.scrollHeight,
      innerHeight,
      scrollY,
      top: box?.top,
      height: box?.height,
      cornerInApp: corner?.closest(selector) !== null && corner !== null,
      overscroll: [getComputedStyle(root).overscrollBehaviorY, getComputedStyle(document.body).overscrollBehaviorY],
    };
  }, app);
  expect(shell.scrollHeight, `${app}: the page scrolls`).toBeLessThanOrEqual(shell.innerHeight);
  expect(shell.bodyHeight).toBeLessThanOrEqual(shell.innerHeight);
  expect(shell.scrollY).toBe(0);
  expect(shell.top).toBe(0);
  expect(Math.round(shell.height ?? 0)).toBe(shell.innerHeight);
  expect(shell.cornerInApp, `${app}: the window shows something past the app`).toBe(true);
  expect(shell.overscroll).toEqual(["none", "none"]);
}

/** Scrolls `pane` to its end, as a wheel over it would. */
async function scrollToEnd(pane: Locator) {
  await pane.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
}

async function scrolls(pane: Locator) {
  return pane.evaluate((element) => element.scrollHeight > element.clientHeight && getComputedStyle(element).overflowY === "auto");
}

async function shoot(page: Page, name: string) {
  if (!shots) return;
  mkdirSync(shots, { recursive: true });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.screenshot({ path: join(shots, `${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: "light" });
}

test("Threads, Today and Memory & rules never scroll as a page, at any size or after a resize", async ({ page }) => {
  await page.setViewportSize(SIZES[0]);
  await page.goto("/ui/fixture.html?section=threads");
  await expect(page.getByRole("heading", { name: "Finance" })).toBeVisible();
  const tabs = page.getByRole("navigation", { name: "Sections" });
  for (const size of SIZES) {
    await page.setViewportSize(size);
    for (const section of ["Today", "Memory & rules", "Threads"]) {
      await tabs.getByRole("link", { name: new RegExp(`^${section}`) }).click();
      await expect(tabs.getByRole("link", { name: new RegExp(`^${section}`) })).toHaveAttribute("aria-current", "page");
      await expectShell(page, ".app");
      await expect(page.getByRole("banner")).toBeInViewport({ ratio: 1 });
    }
  }
});

test("on a desktop the rail, the thread and the right panel scroll inside themselves, the message box in view", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 600 });
  // The right panel outgrows this window only with the morning briefing in it, which the
  // example shows outside its 22:00–07:00 quiet time on the browser's clock.
  await page.clock.setFixedTime(new Date(2026, 9, 26, 12, 0));
  await page.goto("/ui/fixture.html?section=threads");
  await expect(page.getByRole("heading", { name: "Finance" })).toBeVisible();
  const thread = page.locator(".thread-scroll");
  expect(await scrolls(thread)).toBe(true);
  for (const pane of [".rail", ".side"]) {
    expect(await page.locator(pane).evaluate((element) => getComputedStyle(element).overflowY)).toBe("auto");
  }
  expect(await scrolls(page.locator(".side"))).toBe(true);
  const box = page.getByRole("textbox", { name: "Message Finance" });
  await expect(box).toBeInViewport({ ratio: 1 });
  await scrollToEnd(thread);
  await expect(page.getByText("Can't be unsent")).toBeInViewport();
  await expect(box).toBeInViewport({ ratio: 1 });
  await expect(page.getByRole("banner")).toBeInViewport({ ratio: 1 });
  await expectShell(page, ".app");
  await shoot(page, "shell-threads");
});

test("Today and Memory & rules scroll inside their own area, under the header", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 600 });
  for (const section of ["today", "memory"]) {
    await page.goto(`/ui/fixture.html?section=${section}`);
    const content = page.locator("#content");
    await expect(content.getByRole("heading").first()).toBeVisible();
    expect(await scrolls(content)).toBe(true);
    await scrollToEnd(content);
    expect(await content.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
    await expect(page.getByRole("banner")).toBeInViewport({ ratio: 1 });
    await expectShell(page, ".app");
    await shoot(page, `shell-${section}`);
  }
  // Another section starts at its top.
  await page.getByRole("navigation", { name: "Sections" }).getByRole("link", { name: "Today" }).click();
  expect(await page.locator("#content").evaluate((element) => element.scrollTop)).toBe(0);
});

test("under the phone width the column scrolls under the header and the message box stays reachable", async ({ page }) => {
  await page.setViewportSize({ width: 354, height: 807 });
  await page.goto("/ui/fixture.html?section=threads");
  const content = page.locator("#content");
  await expect(content.getByRole("heading", { name: "Finance" })).toBeAttached();
  expect(await scrolls(content)).toBe(true);
  await scrollToEnd(content);
  await expect(page.getByRole("textbox", { name: "Message Finance" })).toBeInViewport({ ratio: 1 });
  await expect(page.getByRole("banner")).toBeInViewport({ ratio: 1 });
  await expectShell(page, ".app");
  await shoot(page, "shell-narrow");
});

test("the tray panel fills its window: its cards scroll inside it, its buttons stay put", async ({ page }) => {
  for (const size of [
    { width: 420, height: 700 },
    { width: 420, height: 460 },
    { width: 1280, height: 600 },
  ]) {
    await page.setViewportSize(size);
    await page.goto("/ui/fixture.html?screen=tray");
    await expect(page.getByRole("article", { name: "Send the October invoice" })).toBeVisible();
    const body = page.locator(".tray-body");
    if (size.height < 700) expect(await scrolls(body)).toBe(true);
    await scrollToEnd(body);
    for (const name of ["Open home", "Stop all"]) {
      await expect(page.getByRole(name === "Stop all" ? "button" : "link", { name })).toBeInViewport({ ratio: 1 });
    }
    await expect(page.getByText("Running")).toBeInViewport();
    await expectShell(page, ".tray");
  }
  // An answered card is one line, which opens and closes in place.
  await page.setViewportSize({ width: 420, height: 700 });
  const invoice = page.getByRole("article", { name: "Send the October invoice" });
  await invoice.getByRole("button", { name: "Allow once" }).click();
  const line = invoice.getByRole("button", { name: /You said yes from the tray at/ });
  await expect(line).toHaveAttribute("aria-expanded", "false");
  await expect(invoice.getByText(/^send_email\(/)).toHaveCount(0);
  await shoot(page, "shell-tray-decided");
  await line.press("Enter");
  await expect(line).toHaveAttribute("aria-expanded", "true");
  await expect(invoice.getByText(/^send_email\(/)).toBeVisible();
  await expectShell(page, ".tray");
});

test("a decided line never cuts off who answered, however long its heading", async ({ page }) => {
  await page.setViewportSize({ width: 420, height: 700 });
  await page.goto("/ui/fixture.html?screen=tray");
  const invoice = page.getByRole("article", { name: "Send the October invoice" });
  await invoice.getByRole("button", { name: "Allow once" }).click();
  const line = invoice.getByRole("button", { name: /You said yes from the tray at/ });
  await expect(line).toBeVisible();
  // A heading as long as the engine writes them: the tool and up to 60 characters of its
  // argument, here an unbroken path, on the tray's narrowest panel.
  const answer = await line.evaluate((button) => {
    const heading = button.querySelector(".decided-heading");
    if (heading) heading.textContent = "Write file reports/2026/october/customer-invoices/acme-corp-final-reconciled.txt";
    const words = button.querySelector(".decided-words")?.getBoundingClientRect();
    const text = button.querySelector(".decided-text");
    const rects = [...(text?.getClientRects() ?? [])];
    return {
      shown: rects.length > 0 && words !== undefined && rects.every((rect) => rect.bottom <= words.bottom + 0.5 && rect.right <= words.right + 0.5),
      headingLines: heading ? Math.round(heading.getBoundingClientRect().height / parseFloat(getComputedStyle(heading).lineHeight)) : 0,
    };
  });
  expect(answer.shown, "the answer is cut off").toBe(true);
  expect(answer.headingLines).toBeLessThanOrEqual(2);
  await shoot(page, "shell-tray-decided-long");
  await expectShell(page, ".tray");
});

test("the phone fills its screen: each screen scrolls inside it above the tabs", async ({ page }) => {
  for (const size of [
    { width: 390, height: 844 },
    { width: 354, height: 420 },
  ]) {
    await page.setViewportSize(size);
    await page.goto("/ui/fixture.html?screen=phone");
    await expect(page.locator(".phone-main").getByRole("heading", { name: /Decisions/ })).toBeVisible();
    await expect(page.getByRole("navigation", { name: "Sections" })).toBeInViewport({ ratio: 1 });
    await expectShell(page, ".phone");
    // The decided card is one line, which opens in place.
    const decided = page.getByRole("article", { name: "Send the October invoice" });
    await expect(decided.getByRole("button", { name: /done once/ })).toHaveAttribute("aria-expanded", "false");
    if (size.height > 600) await shoot(page, "shell-phone");

    // A decision's own screen scrolls inside it, its answers at its end.
    await page.goto("/ui/fixture.html?screen=phone&phone=decide");
    const decide = page.locator(".phone-decide");
    const confirm = page.getByRole("button", { name: /Confirm it's you and approve/ });
    await expect(confirm).toBeAttached();
    if (size.height < 600) expect(await scrolls(decide)).toBe(true);
    await scrollToEnd(decide);
    await expect(confirm).toBeInViewport({ ratio: 1 });
    await expectShell(page, ".phone");
  }
});
