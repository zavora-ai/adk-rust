// Spec 3.5 §8.2 end to end on `scripted_website --live`: a site a week after launch shows its
// site view and takes the owner's decisions, sends its weekly review's summary, and publishes
// and corrects from the web editor.
import { expect, test } from "@playwright/test";
import { png, shoot } from "./website-shared";

test.describe("a site a week after launch", () => {
  test("shows the site view and takes the owner's decisions", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/ui/website");
    await expect(page.getByRole("heading", { name: /Your Juniper Bakery/ })).toBeVisible();
    await expect(page.getByRole("row", { name: /Qualified enquiries/ }).getByText("Better")).toBeVisible();
    await expect(page.getByText("The price moves more than the 10% your price rule allows")).toBeVisible();
    await expect(page.getByText("Over the KES 1,500.00 limit for one payment")).toBeVisible();
    await shoot(page, "10-site-view");
    const releases = page.locator("li.w-release", { hasText: "A correction" });
    await releases.getByRole("button", { name: "1 page changed" }).click();
    await expect(releases.getByText("/posts/rye-week/index.html")).toBeVisible();

    // Autonomy is earned and raised only by the owner (A-I13).
    await page.getByRole("button", { name: "Let it do more: Ask first" }).click();
    await expect(page.getByText(/now works at “Ask first”, by your decision\./)).toBeVisible();
    // A held price change and a held feed delivery are the owner's to decide (A-I25, A-I27).
    await page.locator(".w-held", { hasText: "Sourdough loaf" }).getByRole("button", { name: "Discard" }).click();
    await expect(page.getByText("Discarded; nothing from it is published.")).toBeVisible();
    await page.getByRole("button", { name: "Discard it whole" }).click();
    await expect(page.getByText("The whole delivery is discarded.")).toBeVisible();
  });

  test("sends the weekly review's summary to the owner's channel", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/ui/website/review");
    await expect(page.getByRole("heading", { name: /Weekly review/ })).toBeVisible();
    await expect(page.getByText("Should the site take orders for Saturday collection?")).toBeVisible();
    await shoot(page, "11-weekly-review");
    await page.getByRole("button", { name: "Send the summary to WhatsApp" }).click();
    await expect(page.getByText(/^Sent to WhatsApp on /)).toBeVisible();
  });

  test("writes, previews, publishes and corrects from the web editor", async ({ page }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/ui/website/write");
    await expect(page.getByRole("heading", { name: /Write for your site/ })).toBeVisible();
    await page.getByLabel("New").selectOption("product");
    await page.getByRole("button", { name: "New", exact: true }).click();
    await page.getByLabel("Address").fill("rye-loaf");
    await page.getByLabel("Title").fill("Dark rye loaf");
    await page.getByLabel("Summary").fill("Dense and sour, 1 kg.");
    await page.getByLabel(/^Price\s*\*?$/).fill("520");
    await page.getByLabel("Stock code (SKU)").fill("RY-1000");
    await page.getByLabel("Text").fill("Baked every Tuesday with 100% rye.");
    await page.getByRole("button", { name: "Bold" }).click();
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved. Every check passed.")).toBeVisible();

    // An image through the media pipeline: its description waits for a person (§7.8.2).
    await page.getByLabel("Describe the image").fill("A dark rye loaf on a board");
    await page.getByLabel("Add an image").setInputFiles({ name: "rye.png", mimeType: "image/png", buffer: png() });
    await expect(page.getByRole("button", { name: "This describes it" })).toBeVisible();
    await page.getByRole("button", { name: "This describes it" }).click();
    await expect(page.getByText(/^Description confirmed: /)).toBeVisible();
    await page.getByRole("button", { name: "Preview the changes" }).click();
    await expect(page.locator(".w-page-line", { hasText: "/products/rye-loaf/index.html" }).getByText("New page")).toBeVisible({ timeout: 30_000 });
    await page.getByRole("tab", { name: "Preview" }).click();
    await shoot(page, "12-editor");
    await page.getByRole("button", { name: "Publish", exact: true }).click();
    await expect(page.getByText(/^Live in release \d+, signed\.$/).first()).toBeVisible({ timeout: 30_000 });

    // A correction: a new release with a note readers see.
    const item = page.locator("li.w-item-row", { hasText: "Dark rye loaf" });
    await item.getByRole("button", { name: "Correct" }).click();
    await item.getByLabel("Correction note readers see").fill("The loaf weighs 900 g, not 1 kg.");
    await item.getByRole("button", { name: "Correct" }).click();
    await expect(page.locator("li.w-item-row", { hasText: "Dark rye loaf" }).getByText("Live, corrected")).toBeVisible({ timeout: 30_000 });
  });
});
