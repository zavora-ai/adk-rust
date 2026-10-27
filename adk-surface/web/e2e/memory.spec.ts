// Spec 3.3 §5.1, §8.1, §10 K5, §12 phase 3 end to end, on `adk-rust up` with a scripted model
// (`e2e/memory-kernel.sh`): the home corrects what the agents remember through the kernel —
// any fact fixed or forgotten, a guess confirmed; a rule allowed without asking, with an end
// date, is extended, covers the next call, and once stopped the next identical call waits; and
// "Every action is on record" agrees with `adk-rust audit verify` on an export of the same
// record. Audit records carry ids, never a fact's text.
import { execFileSync } from "node:child_process";
import { createPrivateKey, createPublicKey } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import type { APIRequestContext, Locator, Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

const dir = process.env.MEMORY_E2E_WORKSPACE ?? "";

/** The home the launcher made, and its workspace. */
function home(): { root: string; workspace: string } {
  const root = readFileSync(join(dir, "home"), "utf8").trim();
  return { root, workspace: join(root, "ws") };
}

/** The directory under `root` holding `name`, searched depth first. */
function find(root: string, name: string): string | undefined {
  for (const entry of readdirSync(root)) {
    const path = join(root, entry);
    if (entry === name) return root;
    if (statSync(path).isDirectory()) {
      const found = find(path, name);
      if (found) return found;
    }
  }
  return undefined;
}

/**
 * The verifier key of the kernel's signing key (`<name>+<hash>+<base64(0x01 ‖ public key)>`):
 * the signer key file holds the same name and hash before the seed.
 */
function verifierKey(signerKey: string): string {
  // `PRIVATE+KEY+<name>+<hash>+<base64>`: names hold no `+`, base64 may.
  const [, , name, hash, ...rest] = signerKey.trim().split("+");
  const encoded = rest.join("+");
  const seed = Buffer.from(encoded, "base64").subarray(1);
  const pkcs8 = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]);
  const spki = createPublicKey(createPrivateKey({ key: pkcs8, format: "der", type: "pkcs8" })).export({
    format: "der",
    type: "spki",
  });
  const key = Buffer.concat([Buffer.from([1]), spki.subarray(spki.length - 32)]);
  return `${name}+${hash}+${key.toString("base64")}`;
}

/** `adk-rust audit export` and `audit verify --json` of the kernel's record, as a person runs them. */
function auditVerify(): { valid: boolean; records: number; failures: Array<{ kind: string }>; text: string } {
  const bin = readFileSync(join(dir, "bin"), "utf8").trim();
  const kernelHome = find(home().root, "kernel-id")!;
  const id = readFileSync(join(kernelHome, "kernel-id"), "utf8").trim();
  const out = join(dir, "record.jsonl");
  const env = { PATH: process.env.PATH ?? "", HOME: home().root, RUST_LOG: "off" };
  execFileSync(bin, ["audit", "export", "--database-url", `sqlite://${join(kernelHome, "harness.db")}`, "--log-id", `kernel-${id}`, "--out", out], { env });
  const key = verifierKey(readFileSync(join(kernelHome, "kernel-audit.key"), "utf8"));
  let text: string;
  try {
    text = execFileSync(bin, ["audit", "verify", out, "--key", key, "--json"], { env }).toString();
  } catch (error) {
    // `audit verify` exits 1 when the record does not verify, and prints its report all the same.
    text = (error as { stdout: Buffer }).stdout.toString();
  }
  const report = JSON.parse(text) as { records: number; failures: Array<{ kind: string }> };
  return { valid: report.failures.length === 0, records: report.records, failures: report.failures, text: readFileSync(out, "utf8") };
}

function section(page: Page, heading: string): Locator {
  return page.locator("section", { has: page.getByRole("heading", { name: heading }) });
}

async function memory(request: APIRequestContext) {
  return (await (await request.get("/api/kernel/memory")).json()) as {
    facts: Array<{ id: string; text: string; origin: string }>;
  };
}

/** Sends `text` to the agent from Threads, as the person types it. */
async function send(page: Page, text: string) {
  await page.getByRole("link", { name: /Threads/ }).click();
  const box = page.getByRole("main").getByRole("textbox");
  await box.fill(text);
  await box.press("Enter");
}

test("Memory and rules corrects the memory, extends and stops a rule, and agrees with audit verify", async ({ page, request }) => {
  test.setTimeout(180_000);
  const started = Date.now();
  const { workspace } = home();

  // What your agents remember: the session's facts, with where each came from.
  await page.goto("/ui/memory");
  const remember = section(page, "What your agents remember");
  const fact = (text: string) => remember.locator("li.fact", { hasText: text });
  await expect(fact("You bill [CLIENT] at [RATE] an hour").getByText("You said this")).toBeVisible({ timeout: 20_000 });
  await expect(remember.getByText("A guess")).toHaveCount(3);

  // That's right: the guess becomes "You said this" (spec 3.3 §8.1, §12 phase 3).
  await fact("Your accountant is [NAME]").getByRole("button", { name: "That's right" }).click();
  await expect(fact("Your accountant is [NAME]").getByText("You said this")).toBeVisible();

  // Fix it: the person's words replace the guess.
  await fact("You send invoices on the last working day").getByRole("button", { name: "Fix it" }).click();
  const words = remember.getByRole("textbox", { name: "Your words" });
  await words.fill("You send invoices on the first working day");
  await words.press("Enter");
  await expect(fact("You send invoices on the first working day").getByText("You said this")).toBeVisible();
  await expect(remember.getByText("You send invoices on the last working day")).toHaveCount(0);

  // Forget it: the note is gone.
  await fact("Your bank is [BANK]").getByRole("button", { name: "Forget it" }).click();
  await expect(remember.getByRole("status")).toHaveText("Forgotten. Your agents won't use it from their next step.");
  await expect(remember.getByText("Your bank is [BANK]")).toHaveCount(0);
  // What the person said or confirmed is fixed and forgotten the same way; only a guess is
  // confirmed.
  await expect(fact("You bill [CLIENT] at [RATE] an hour").getByRole("button", { name: "That's right" })).toHaveCount(0);
  await fact("You bill [CLIENT] at [RATE] an hour").getByRole("button", { name: "Fix it" }).click();
  await words.fill("You bill [CLIENT] at [NEW RATE] an hour");
  await words.press("Enter");
  await expect(fact("You bill [CLIENT] at [NEW RATE] an hour").getByText("You said this")).toBeVisible();
  await expect(remember.getByText("You bill [CLIENT] at [RATE] an hour")).toHaveCount(0);
  await fact("Your accountant is [NAME]").getByRole("button", { name: "Forget it" }).click();
  await expect(remember.getByText("Your accountant is [NAME]")).toHaveCount(0);
  const facts = (await memory(request)).facts;
  expect(facts.map((entry) => [entry.text, entry.origin]).sort()).toEqual(
    [
      ["You bill [CLIENT] at [NEW RATE] an hour", "youConfirmedThis"],
      ["You send invoices on the first working day", "youConfirmedThis"],
    ].sort(),
  );

  // A rule with an end date, as the terminal makes one: write_file within notes/, for a day.
  const created = (await (
    await request.post("/api/rules", { data: { tool: "write_file", pathWithin: "notes", expiresInHours: 24 } })
  ).json()) as { rule: { id: string; expiresAt: string } };
  await page.reload();
  const canDo = section(page, "What your agents can do");
  const rule = canDo.locator("li.rule", { hasText: "Your agents can use write_file within notes/." });
  await expect(rule.getByText(/^Until \d+ \w+$/)).toBeVisible({ timeout: 20_000 });

  // Extend: forty days on, shown as was and now, and kept by the harness.
  await rule.getByRole("button", { name: "Extend" }).click();
  const later = new Date(Date.now() + 40 * 86_400_000);
  const value = `${later.getFullYear()}-${String(later.getMonth() + 1).padStart(2, "0")}-${String(later.getDate()).padStart(2, "0")}`;
  await rule.getByLabel("Until the end of").fill(value);
  await rule.getByRole("button", { name: "Extend" }).click();
  await expect(rule.getByRole("status")).toHaveText(/^Was until \d+ \w+\. Now until \d+ \w+\.$/);
  const rules = (await (await request.get("/api/rules")).json()) as { rules: Array<{ id: string; expiresAt: string }> };
  const extended = rules.rules.find((entry) => entry.id === created.rule.id)!;
  expect(new Date(extended.expiresAt).getTime()).toBeGreaterThan(new Date(created.rule.expiresAt).getTime() + 30 * 86_400_000);

  // The rule covers the next call: it runs without asking.
  await send(page, "Write note a");
  await expect(page.getByRole("main").getByText("Note a is written.")).toBeVisible({ timeout: 30_000 });
  expect(readFileSync(join(workspace, "notes", "a.txt"), "utf8")).toBe("A");
  await expect(page.getByRole("heading", { name: /^Decisions/ }).locator(".badge")).toHaveCount(0);

  // Stop allowing: the next identical call waits for the person (§12 phase 3).
  await page.getByRole("link", { name: "Memory & rules" }).click();
  await canDo.locator("li.rule", { hasText: "write_file within notes/" }).getByRole("button", { name: "Stop allowing" }).click();
  await expect(canDo.locator("li.rule", { hasText: "write_file within notes/" })).toHaveCount(0);
  const after = (await (await request.get("/api/rules")).json()) as { rules: Array<{ id: string }> };
  expect(after.rules.map((entry) => entry.id)).not.toContain(created.rule.id);
  await send(page, "Write note a again");
  const card = page.getByRole("main").getByRole("article").last();
  await expect(card.getByRole("button", { name: "Allow once" })).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("heading", { name: /^Decisions/ }).locator(".badge")).toHaveText("1");
  await card.getByRole("button", { name: "Deny" }).click();
  await expect(page.getByRole("main").getByText("I did not write it.")).toBeVisible({ timeout: 30_000 });

  // What your agents did: the latest actions as sentences, and each agent's spending adds up.
  await page.getByRole("link", { name: "Memory & rules" }).click();
  const did = section(page, "What your agents did");
  // The answer names whose call it was (§11).
  await expect(did.getByText("You didn't let ADK-Rust use write_file, in the home.")).toBeVisible({ timeout: 20_000 });
  await expect(did.getByText("ADK-Rust used write_file.")).toBeVisible();
  const roster = canDo.getByRole("region", { name: "What each agent spent today" });
  await expect(roster.getByText("ADK-Rust")).toBeVisible();
  const amounts = (await roster.locator(".area-agent-spent").allTextContents()).map((text) => Math.round(Number(text.replace(/[^\d.]/g, "")) * 100));
  const spent = (await canDo.locator(".setting-sub", { hasText: / spent today$/ }).textContent())!;
  expect(amounts.reduce((sum, cents) => sum + cents, 0)).toBe(Math.round(Number(spent.replace(/[^\d.]/g, "")) * 100));

  // The record's check is the one `adk-rust audit verify` makes on an export of it: the same
  // count, valid or not alike. Records the log has not signed yet read as such, never as
  // changed; once the log signs a checkpoint (a minute after the last, on its next record), the
  // home reads "nothing has been changed or deleted" exactly when the command says VALID with
  // no unsigned tail, and otherwise names the records signed since.
  const agree = async (signed: boolean) => {
    for (let attempt = 0; attempt < 5; attempt++) {
      await did.getByRole("button", { name: "Check again" }).click();
      const check = (await (await request.get("/api/kernel/record/check")).json()) as {
        valid: boolean;
        entries: number;
        report: { unsignedTail: number };
      };
      const verified = auditVerify();
      if (verified.records !== check.entries) continue;
      expect(verified.valid).toBe(check.valid);
      expect(check.valid).toBe(signed);
      const shown = did.locator(".record-sub").first();
      await expect(shown).toContainText(`All ${check.entries.toLocaleString("en-US")} of them`);
      await expect(did.getByText("Every action is on record")).toBeVisible();
      await expect(did.getByText("Something on record was changed or deleted.")).toHaveCount(0);
      if (signed && check.report.unsignedTail === 0) {
        await expect(shown).toContainText("nothing has been changed or deleted");
      } else if (signed) {
        await expect(shown).toContainText(`the latest ${check.report.unsignedTail.toLocaleString("en-US")} are not signed yet`);
      } else {
        expect(verified.failures.every((failure) => failure.kind === "unanchored")).toBe(true);
        await expect(shown).toContainText("signed yet");
      }
      // The record names facts by id, never by their text.
      for (const text of ["Your bank is [BANK]", "You send invoices on the last working day", "[NAME]", "[NEW RATE]", "[RATE]"]) {
        expect(verified.text).not.toContain(text);
      }
      return;
    }
    throw new Error("the record kept growing between the check and the export");
  };
  await agree(false);
  await page.waitForTimeout(Math.max(0, 62_000 - (Date.now() - started)));
  // A change of the daily spending limit is the record's next entry, and signs a checkpoint.
  await canDo.locator("li.setting", { hasText: "Daily spending limit" }).getByRole("button", { name: "Change" }).click();
  await canDo.getByLabel("Dollars a day").fill("5");
  await canDo.getByRole("button", { name: "Save" }).click();
  await expect(canDo.getByText(/^Was No limit\. Now \$5\.00\.$/)).toBeVisible();
  await agree(true);
  expect(existsSync(join(workspace, "notes", "a.txt"))).toBe(true);
});
