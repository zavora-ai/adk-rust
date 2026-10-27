// Spec 3.3 §12 developer track, "Approve, rewind, fork", end to end over `scripted_workflow`:
// the invoice-run graph waits at its charge, is approved from the Runs tab and charges once; a
// step is read back without changing anything; a fork from the step before the charge is
// answered from the record, and the provider still counts one charge. `WORKFLOW_E2E_SHOTS`
// names a folder for light and dark screenshots of each screen.
import { join } from "node:path";
import type { APIRequestContext, Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

const shots = process.env.WORKFLOW_E2E_SHOTS;
const SESSION = "s-7f21";

test.use({ viewport: { width: 1440, height: 1100 } });

/** `page` as it looks in light and in dark, when screenshots are asked for. */
async function shoot(page: Page, name: string) {
  if (!shots) return;
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.screenshot({ path: join(shots, `web-workflow-${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: "light" });
}

async function hits(request: APIRequestContext): Promise<number> {
  return ((await (await request.get("/hits")).json()) as { hits: number }).hits;
}

async function run(request: APIRequestContext, session: string) {
  return (await (await request.get(`/api/surface/workflow/sessions/${session}/run`)).json()) as {
    status: string;
    steps: { step: number; checkpointId?: string }[];
  };
}

test("approve, rewind and fork a graph run; the charge is sent once", async ({ page, request }) => {
  const sent = await request.post("/api/surface/commands", {
    data: {
      surface: "web",
      origin: { kind: "pointer" },
      command: { type: "newThread", agent: "invoice-run", sessionId: SESSION, text: "Bill Acme for October" },
    },
  });
  expect(sent.ok()).toBe(true);
  await expect.poll(async () => (await run(request, SESSION)).status, { timeout: 20_000 }).toBe("waiting");

  // 1. The run waits at pay; its card is the inbox's, with no Always allow.
  await page.goto(`/ui/developer/runs/${SESSION}`);
  const runs = page.locator(".runs");
  await expect(runs.locator(".runs-head")).toContainText("Waiting for you");
  await expect(page.getByRole("button", { name: /Runs/ }).locator(".tab-badge")).toHaveText("1");
  const card = page.getByRole("article", { name: "Charge Acme 42.00 USD" });
  await expect(card).toBeVisible();
  await expect(card.getByRole("button", { name: "Confirm it's you and approve" })).toBeVisible();
  await expect(card.getByRole("button", { name: /Always allow/ })).toHaveCount(0);
  await expect(card.getByText("From the invoice-run workflow")).toBeVisible();
  expect(await hits(request)).toBe(0);
  await shoot(page, "run");

  // The waiting step: its node links to the decision it waits on.
  await page.locator(".step-row").nth(2).locator(".step-open").click();
  await expect(page.locator(".step-detail")).toContainText("Open in Decisions");
  await shoot(page, "step-waiting");
  await page.getByRole("button", { name: "Back to steps" }).click();

  // 2. Approved from the Runs tab: the run finishes and the provider counts one charge.
  await card.getByRole("button", { name: "Confirm it's you and approve" }).click();
  await expect(runs.locator(".runs-head")).toContainText("Done", { timeout: 20_000 });
  expect(await hits(request)).toBe(1);
  const before = await run(request, SESSION);
  await shoot(page, "run-done");

  // 4. Rewind is read-only: the step after draft, then the charge's step, read back.
  await page.locator(".step-row").nth(2).locator(".step-open").click();
  await expect(page.locator(".step-detail")).toContainText("You said yes");
  await expect(page.locator(".changes")).toContainText("charge");
  await shoot(page, "step");
  await page.getByRole("button", { name: "Back to steps" }).click();

  // 5. Fork from step 1 with the note edited: answered from the record, not sent again.
  const drafted = page.locator(".step-row").nth(1);
  await drafted.hover();
  await drafted.getByRole("button", { name: "Fork from here" }).click();
  const dialog = page.getByRole("dialog", { name: "Fork from step 1" });
  await expect(dialog).toContainText("Runs again: pay, receipt");
  await dialog.getByRole("button", { name: "Edit" }).nth(2).click();
  await dialog.getByRole("textbox", { name: "note" }).fill('"re-run for audit"');
  await expect(dialog.getByText("Changed", { exact: true })).toBeVisible();
  await shoot(page, "fork");
  await dialog.getByRole("button", { name: "Fork", exact: true }).click();
  await expect(page.getByText("Forked. The original run is unchanged.")).toBeVisible();
  await expect(runs.locator(".lineage-chip")).toContainText("edited note");
  await expect(runs.locator(".runs-head")).toContainText("Done", { timeout: 20_000 });
  await expect(page.locator(".run-graph")).toHaveAttribute("aria-label", /pay Not sent again/);
  expect(await hits(request)).toBe(1);
  await shoot(page, "fork-done");

  // The original run is unchanged; only its list of forks grew.
  const { forks, ...after } = (await run(request, SESSION)) as Awaited<ReturnType<typeof run>> & { forks?: unknown[] };
  const { forks: none, ...unchanged } = before as typeof before & { forks?: unknown[] };
  expect([after, forks?.length, none?.length]).toEqual([unchanged, 1, undefined]);

  // A fork that changes the charge waits for a person: its card, in the thread, says where it
  // came from. Denied, nothing is sent.
  const c1 = before.steps.find((step) => step.step === 1)?.checkpointId ?? "";
  const changed = await request.post(`/api/surface/workflow/sessions/${SESSION}/fork`, {
    data: { from: c1, set: { invoice: { customer: "Acme", amountCents: 5000 } }, start: true },
  });
  const fork = (await changed.json()) as { sessionId: string };
  await expect.poll(async () => (await run(request, fork.sessionId)).status, { timeout: 20_000 }).toBe("waiting");
  const open = (await (await request.get(`/api/surface/decisions?session=${fork.sessionId}`)).json()) as {
    decisions: { id: string }[];
  };
  await page.goto(`/ui/decisions/${encodeURIComponent(open.decisions[0].id)}`);
  const changedCard = page.getByRole("main").getByRole("article", { name: "Charge Acme 50.00 USD" });
  await expect(changedCard).toBeVisible({ timeout: 20_000 });
  await expect(changedCard.getByRole("link", { name: "Open in Runs" })).toBeVisible();
  await shoot(page, "decision");
  await changedCard.getByRole("button", { name: "Deny" }).click();
  await expect.poll(async () => (await run(request, fork.sessionId)).status, { timeout: 20_000 }).toBe("failed");
  expect(await hits(request)).toBe(1);
});
