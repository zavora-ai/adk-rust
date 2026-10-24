import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineConfig } from "@playwright/test";

// The end-to-end test opens the web home served by a scripted kernel
// (`adk-surface/examples/scripted_home.rs`, spec 3.3 §5.1). Each run starts the example in a
// new, empty workspace, so a rerun never finds the files of the last one. `HOME_E2E_BIN`
// starts an example already built instead of `cargo run`. `HOME_E2E_URL` points the test at
// a home already running, with `HOME_E2E_WORKSPACE` naming that server's workspace.
const port = Number(process.env.HOME_E2E_PORT ?? 8791);
if (process.env.HOME_E2E_URL && !process.env.HOME_E2E_WORKSPACE) {
  throw new Error("HOME_E2E_URL needs HOME_E2E_WORKSPACE: the workspace that server writes to");
}
// The runner's workers load this file again and keep the workspace the first load made.
export const workspace = process.env.HOME_E2E_WORKSPACE ?? mkdtempSync(join(tmpdir(), "adk-home-e2e-"));
process.env.HOME_E2E_WORKSPACE = workspace;
const example = process.env.HOME_E2E_BIN ?? "cargo run -p adk-surface --features web --example scripted_home --";

export default defineConfig({
  testDir: "e2e",
  timeout: 60_000,
  retries: 0,
  workers: 1,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: process.env.HOME_E2E_URL ?? `http://127.0.0.1:${port}`,
    // A local run uses the installed Chrome; CI installs Playwright's Chromium.
    ...(process.env.CI ? {} : { channel: "chrome" }),
    trace: "retain-on-failure",
  },
  webServer: process.env.HOME_E2E_URL
    ? undefined
    : {
        command: `${example} ${port} ${workspace}`,
        cwd: "../..",
        url: `http://127.0.0.1:${port}/ui/`,
        reuseExistingServer: false,
        timeout: 20 * 60_000,
        stdout: "pipe",
      },
});
