import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineConfig } from "@playwright/test";
import type { PlaywrightTestConfig } from "@playwright/test";

// Each surface's end-to-end tests run as one project against its own scripted kernel, an
// example of adk-surface on its own port (spec 3.3 §5). Each run starts every example in a new,
// empty workspace, so a rerun never finds the files of the last one. Per project, with its
// prefix (`HOME`, …): `<PREFIX>_E2E_PORT` moves the port; `<PREFIX>_E2E_BIN` starts an example
// already built instead of `cargo run`; `<PREFIX>_E2E_URL` points the project at a server
// already running, with `<PREFIX>_E2E_WORKSPACE` naming that server's workspace.
interface Surface {
  /** The project's name: `npx playwright test --project <name>`. */
  name: string;
  /** The prefix of its environment variables. */
  prefix: string;
  /** The adk-surface example it runs against. */
  example: string;
  /** Its default port. */
  port: number;
  /** Its tests under `e2e/`. */
  testMatch: string[];
  /** The page the server is ready at. */
  ready: string;
}

const surfaces: Surface[] = [
  {
    name: "home",
    prefix: "HOME",
    example: "scripted_home",
    port: 8791,
    testMatch: ["held-call.spec.ts", "today.spec.ts"],
    ready: "/ui/",
  },
];

function env(surface: Surface, name: string): string | undefined {
  return process.env[`${surface.prefix}_E2E_${name}`];
}

/** Where each project's server runs and what it writes to. */
const servers = surfaces.map((surface) => {
  const url = env(surface, "URL");
  if (url && !env(surface, "WORKSPACE")) {
    throw new Error(`${surface.prefix}_E2E_URL needs ${surface.prefix}_E2E_WORKSPACE: the workspace that server writes to`);
  }
  // The runner's workers load this file again and keep the workspace the first load made.
  const workspace = env(surface, "WORKSPACE") ?? mkdtempSync(join(tmpdir(), `adk-${surface.name}-e2e-`));
  process.env[`${surface.prefix}_E2E_WORKSPACE`] = workspace;
  const port = Number(env(surface, "PORT") ?? surface.port);
  const command = env(surface, "BIN") ?? `cargo run -p adk-surface --features web --example ${surface.example} --`;
  return { surface, url: url ?? `http://127.0.0.1:${port}`, external: url !== undefined, port, workspace, command };
});

const webServer: PlaywrightTestConfig["webServer"] = servers
  .filter((server) => !server.external)
  .map((server) => ({
    command: `${server.command} ${server.port} ${server.workspace}`,
    cwd: "../..",
    url: `${server.url}${server.surface.ready}`,
    reuseExistingServer: false,
    timeout: 20 * 60_000,
    stdout: "pipe" as const,
  }));

export default defineConfig({
  testDir: "e2e",
  timeout: 60_000,
  retries: 0,
  workers: 1,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    // A local run uses the installed Chrome; CI installs Playwright's Chromium.
    ...(process.env.CI ? {} : { channel: "chrome" }),
    trace: "retain-on-failure",
  },
  projects: servers.map((server) => ({
    name: server.surface.name,
    testMatch: server.surface.testMatch,
    use: { baseURL: server.url },
  })),
  webServer,
});
