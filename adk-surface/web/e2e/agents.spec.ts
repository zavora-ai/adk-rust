// Spec 3.3 §5.7 "Hire a helper" end to end, on `adk-rust up` with a scripted model
// (`e2e/agents-kernel.sh`): Describe it asks ADK-Rust, which suggests Finance; nothing is added
// until a person says yes on its card; Finance is Ready in the roster and answers in a thread of
// its own; renamed in its settings with "Was … Now …"; removed, naming what ends; an agent set up
// by hand is added from its preview.
import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

function roster(page: Page) {
  return page.getByRole("navigation", { name: "Agents" });
}

function row(page: Page, name: string) {
  return roster(page).getByRole("button", { name: new RegExp(`^${name}\\b`) });
}

test("a person adds the agent ADK-Rust suggests, works with it, renames it and removes it", async ({ page }) => {
  await page.goto("/ui/");
  await roster(page).getByRole("button", { name: "New agent" }).click();
  await expect(page).toHaveURL(/\/ui\/agents\/new/);

  // Describe it: a new thread with ADK-Rust, which suggests Finance.
  await page.getByLabel("What should it do?").fill("Read my invoices and draft payment reminders");
  await page.getByRole("button", { name: "Ask ADK-Rust to suggest one" }).click();
  const card = page.getByRole("article", { name: "Add an agent called Finance" });
  await expect(card).toBeVisible({ timeout: 30_000 });
  await expect(card.getByText("Reads files in Invoices")).toBeVisible();
  await expect(card.getByText("Nothing yet")).toBeVisible();
  await expect(card.getByRole("button", { name: /Always allow/ })).toHaveCount(0);
  await expect(row(page, "Finance")).toHaveCount(0);

  // Add this agent: Finance is Ready, beside ADK-Rust.
  await card.getByRole("button", { name: "Add this agent" }).click();
  await expect(row(page, "Finance")).toContainText("Ready", { timeout: 15_000 });

  // Its own thread: the message runs as Finance.
  await row(page, "Finance").click();
  await page.getByLabel("Message Finance").fill("hello");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByText("Finance here.")).toBeVisible({ timeout: 30_000 });

  // Settings: renamed Bills, shown as Was … Now … before it is saved.
  await row(page, "Finance").hover();
  await roster(page).getByRole("button", { name: "Settings: Finance" }).click();
  await expect(page).toHaveURL(/\/ui\/agents\/finance/);
  await page.getByLabel("Name").fill("Bills");
  await expect(page.getByLabel("Was · Now")).toContainText("Bills");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Bills" })).toBeVisible({ timeout: 15_000 });

  // Remove, which names what ends first; its threads stay.
  await page.getByRole("button", { name: "Remove", exact: true }).click();
  const confirm = page.getByRole("alertdialog", { name: "Remove Bills?" });
  await expect(confirm).toContainText("Its threads stay.");
  await confirm.getByRole("button", { name: "Remove", exact: true }).click();
  await expect(page.getByText("You removed Bills. Its threads stay.")).toBeVisible();
  await expect(row(page, "Bills")).toHaveCount(0);
});

test("an agent set up by hand is added from what it can do", async ({ page }) => {
  await page.goto("/ui/agents/new");
  await page.getByRole("tab", { name: "Set it up yourself" }).click();
  await page.getByLabel("Name").fill("Travel");
  await page.getByLabel("What it does").fill("Finds train times and drafts trip plans");
  await page.getByLabel("Instructions").fill("Draft a plan for each trip in the calendar. Never book anything.");
  await page.getByRole("radiogroup", { name: "How much it asks first" }).getByRole("radio", { name: "Suggest" }).click();
  await expect(page.getByText("Here is what Travel can do")).toBeVisible();
  await expect(page.getByText("Low risk")).toBeVisible();
  await page.getByRole("button", { name: "Add Travel" }).click();
  // Its settings open; back in the threads, it is Ready in the roster.
  await expect(page.getByRole("heading", { level: 1, name: "Travel" })).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: "Back to your threads" }).click();
  await expect(row(page, "Travel")).toContainText("Ready", { timeout: 15_000 });
});
