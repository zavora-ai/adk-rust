// Spec 3.3 §5.5, §7.2, §7.4, §11 S-I2, S-I10, §12 phase 5 end to end on the scripted tray
// (`adk-surface/examples/scripted_tray.rs --code-turn`): the panel's badge, yes once from the
// tray for an everyday call and the home for approving a high-risk one, a message queued behind a card,
// keys that never decide, and Stop all without Resume all.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";

const workspace = process.env.TRAY_E2E_WORKSPACE ?? "";
const shots = process.env.TRAY_E2E_SHOTS;

test("glance, say yes once, stop", async ({ page, request, context }) => {
  await page.setViewportSize({ width: 420, height: 700 });

  // Code's held release push is there from the start; Finance gets a message, as the home
  // sends it.
  const send = await request.post("/api/surface/commands", {
    data: {
      surface: "web",
      origin: { kind: "keyboard" },
      command: { type: "send", sessionId: "finance-october", text: "Send the October invoice" },
    },
  });
  expect(send.ok()).toBe(true);

  // a. The panel: running, spend against the limit, quiet time and the badge.
  await page.goto("/ui/tray");
  const decisions = page.getByRole("heading", { name: /^Decisions/ });
  await expect(decisions.locator(".tray-badge")).toHaveText("2", { timeout: 30_000 });
  await expect(page.getByText("Running")).toBeVisible();
  await expect(page.getByText("$0.00 of $5.00 today")).toBeVisible();
  await expect(page.getByText(/^Quiet (from|until) /)).toBeVisible();

  // b. The Finance card offers yes once or no; the Code card is approved only in the home.
  const finance = page.locator("article", { hasText: "write_file" });
  await expect(finance.getByRole("button")).toHaveText(["Allow once", "Deny"]);
  await expect(finance.getByText(/^Finance · since \d\d:\d\d · Medium risk$/)).toBeVisible();
  await expect(finance.getByText("invoice.txt", { exact: false }).first()).toBeVisible();
  // Saying no is safe anywhere, so the engine leaves the tray Deny; approving needs the home.
  const code = page.locator("article", { hasText: "publish_release" });
  await expect(code.getByRole("button")).toHaveText(["Deny"]);
  await expect(code.getByText("Code · High risk · open to decide")).toBeVisible();
  expect(existsSync(join(workspace, "invoice.txt"))).toBe(false);
  if (shots) {
    for (const theme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: theme });
      await page.screenshot({ path: join(shots, `tray-live-${theme}.png`) });
    }
    await page.emulateMedia({ colorScheme: "light" });
  }

  const open = (await (await request.get("/api/surface/decisions?surface=tray")).json()).decisions as Array<{
    id: string;
    call: { tool: string };
    sessionId: string;
  }>;
  const release = open.find((card) => card.call.tool === "publish_release")!;
  const invoice = open.find((card) => card.call.tool === "write_file")!;
  await expect(code.getByRole("link").first()).toHaveAttribute("href", `/ui/decisions/${encodeURIComponent(release.id)}`);
  const [home] = await Promise.all([context.waitForEvent("page"), code.getByRole("link").first().click()]);
  await expect(home.getByRole("button", { name: "Confirm it's you and approve" })).toBeVisible({ timeout: 20_000 });
  await expect(home).toHaveURL(/\/ui\/$/);
  await home.close();

  // c. A message to Finance while its card is open waits behind it.
  const ask = page.getByRole("textbox", { name: "Ask Finance" });
  await ask.fill("and the November one");
  await ask.press("Enter");
  await expect(page.getByText("Sent after this card is answered")).toBeVisible();

  // d. Enter and Esc on the focused answer never decide (§7.4).
  await finance.getByRole("button", { name: "Allow once" }).focus();
  await page.keyboard.press("Enter");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(500);
  await expect(finance.getByRole("button", { name: "Allow once" })).toBeVisible();
  expect(existsSync(join(workspace, "invoice.txt"))).toBe(false);

  // e. Allow once: the file is written once, the badge shows 1, and the record says "from the tray".
  await finance.getByRole("button", { name: "Allow once" }).click();
  await expect(finance.getByText(/^You said yes from the tray at \d\d:\d\d$/)).toBeVisible({ timeout: 20_000 });
  await expect(decisions.locator(".tray-badge")).toHaveText("1", { timeout: 20_000 });
  await expect.poll(() => existsSync(join(workspace, "invoice.txt")), { timeout: 20_000 }).toBe(true);
  expect(readFileSync(join(workspace, "invoice.txt"), "utf8")).toBe("INV-1");
  const decided = await (await request.get(`/api/surface/decisions/${encodeURIComponent(invoice.id)}?surface=web`)).json();
  expect(decided.decided.via.surface).toBe("tray");
  expect(decided.decided.text).toMatch(/^You said yes from the tray at \d\d:\d\d$/);

  // The home's card says the same.
  const homeCard = await context.newPage();
  await homeCard.goto(`/ui/decisions/${encodeURIComponent(invoice.id)}`);
  await expect(homeCard.getByRole("main").getByText(/You said yes from the tray at/).first()).toBeVisible({ timeout: 20_000 });
  await homeCard.close();

  // Exactly once, and the queued message went after the answer.
  const thread = async () =>
    (await (await request.get(`/api/surface/sessions/${encodeURIComponent(invoice.sessionId)}/thread?surface=web`)).json())
      .entries as Array<{ items: Array<{ kind: string; tool?: string; text?: string }> }>;
  await expect
    .poll(async () => (await thread()).flatMap((entry) => entry.items).some((item) => item.text === "and the November one"), {
      timeout: 20_000,
    })
    .toBe(true);
  const results = (await thread()).flatMap((entry) => entry.items).filter((item) => item.kind === "result" && item.tool === "write_file");
  expect(results).toHaveLength(1);

  // f. Stop all from the tray stops everything within about a second; the tray never resumes.
  const stopped = Date.now();
  await page.getByRole("button", { name: "Stop all" }).click();
  await expect
    .poll(async () => (await (await request.get("/api/surface/status?surface=tray")).json()).stopped, { timeout: 1_500, intervals: [50] })
    .toBe(true);
  expect(Date.now() - stopped).toBeLessThan(1_500);
  await expect(page.getByText("All agents stopped")).toBeVisible();
  await expect(page.getByRole("button", { name: "Resume all" })).toHaveCount(0);
  const resume = await request.post("/api/surface/commands", {
    data: { surface: "tray", origin: { kind: "pointer" }, command: { type: "resumeAll" } },
  });
  expect(resume.status()).toBe(403);
  expect(JSON.stringify(await resume.json())).toContain("surface.not_here");
});
