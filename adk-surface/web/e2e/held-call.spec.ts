// Spec 3.3 §5.1, §7, §11 S-I2 end to end: open the home, see the call the scripted agent's
// harness holds, allow it once, and see it run exactly once.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";

const workspace = process.env.HOME_E2E_WORKSPACE ?? "";

test("a held call, allowed once from the home, runs once", async ({ page, request }) => {
  await page.goto("/ui/");
  await expect(page.getByRole("button", { name: "Stop all" })).toBeVisible();
  await expect(page.getByText("No agent has worked yet.")).toBeVisible();

  const box = page.getByRole("textbox", { name: "Message your agents" });
  await box.fill("Write the October invoice");
  await box.press("Enter");

  // The harness holds the write; the card is drawn from the inbox entry, in the thread and
  // in Decisions with a filled count badge.
  const card = page.getByRole("main").getByRole("article", { name: "Write file invoice.txt" });
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card.getByText("write_file")).toBeVisible();
  await expect(page.getByRole("heading", { name: /^Decisions/ }).locator(".badge")).toHaveText("1");
  await expect(page.getByRole("navigation", { name: "Agents" }).getByText("Waiting for you")).toBeVisible();
  expect(existsSync(join(workspace, "invoice.txt"))).toBe(false);

  // Enter on the focused answer never decides (§7.4).
  await card.getByRole("button", { name: "Allow once" }).focus();
  await page.keyboard.press("Enter");
  await page.waitForTimeout(500);
  await expect(card.getByRole("button", { name: "Allow once" })).toBeVisible();

  await card.getByRole("button", { name: "Allow once" }).click();

  // The parked process wakes, the tool runs, and the agent answers.
  await expect(page.getByRole("main").getByText("The invoice is written.")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("heading", { name: /^Decisions/ }).locator(".badge")).toHaveCount(0);
  expect(readFileSync(join(workspace, "invoice.txt"), "utf8")).toBe("INV-1");

  // Exactly once: one result for the write in the session's thread.
  const decisions = await (await request.get("/api/surface/decisions?surface=web")).json();
  expect(decisions.decisions).toEqual([]);
  const agents = await (await request.get("/api/surface/agents?surface=web")).json();
  const session = agents.agents[0].sessionId as string;
  const thread = await (await request.get(`/api/surface/sessions/${encodeURIComponent(session)}/thread?surface=web`)).json();
  const results = thread.entries
    .flatMap((entry: { items: { kind: string; tool?: string }[] }) => entry.items)
    .filter((item: { kind: string; tool?: string }) => item.kind === "result" && item.tool === "write_file");
  expect(results).toHaveLength(1);
});
