import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineConfig } from "@playwright/test";
import type { PlaywrightTestConfig } from "@playwright/test";

// Each surface's end-to-end tests run as one project against its own scripted kernel, an
// example of adk-surface on its own port (spec 3.3 §5). Each run starts every example in a new,
// empty workspace, so a rerun never finds the files of the last one. Per project, with its
// prefix (`HOME`, `TRAY`, `MEMORY`, …): `<PREFIX>_E2E_PORT` moves the port; `<PREFIX>_E2E_BIN`
// starts an example already built instead of `cargo run`; `<PREFIX>_E2E_URL` points the project
// at a server already running, with `<PREFIX>_E2E_WORKSPACE` naming that server's workspace.
interface Surface {
  /** The project's name: `npx playwright test --project <name>`. */
  name: string;
  /** The prefix of its environment variables. */
  prefix: string;
  /** The adk-surface example it runs against, unless `command` serves it. */
  example?: string;
  /** The command that serves it instead of an example, given the port and the workspace. */
  command?: string;
  /** The adk-surface features the example needs; `web` unless named. */
  features?: string;
  /** The host the page is opened at: `127.0.0.1` unless named; `localhost` where WebAuthn needs a domain. */
  host?: string;
  /** Its default port. */
  port: number;
  /** Its tests under `e2e/`. */
  testMatch: string[];
  /** The page the server is ready at. */
  ready: string;
  /** The example's arguments after its port and workspace. */
  args?: string;
  /** What the project's browser runs with. */
  use?: PlaywrightTestConfig["use"];
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
  {
    name: "tray",
    prefix: "TRAY",
    example: "scripted_tray",
    port: 8792,
    testMatch: ["tray.spec.ts"],
    ready: "/ui/tray",
    args: "--code-turn",
  },
  {
    // Memory and rules over `adk-rust up` with a scripted model (`e2e/memory-kernel.sh`), whose
    // harness keeps the facts, the standing rules and the signed record the page reads.
    name: "memory",
    prefix: "MEMORY",
    command: "bash adk-surface/web/e2e/memory-kernel.sh",
    // 8792 is taken by the tray and the voice view.
    port: 8796,
    testMatch: ["memory.spec.ts"],
    ready: "/ui/",
  },
  {
    // The phone pairs, sets up its passkey and confirms a high-risk call (spec 3.3 §5.4,
    // §8.2): a passkey's relying party is the page's domain, so the page is opened at
    // `localhost`, never an IP address.
    name: "phone",
    prefix: "PHONE",
    example: "scripted_phone",
    features: "mobile",
    host: "localhost",
    port: 8799,
    testMatch: ["phone.spec.ts"],
    ready: "/ui/",
  },
  {
    // The app shell's layout over the web home's example (`e2e/fixture-server.sh`): no page
    // scroll at any size, panes that scroll inside themselves, the tray panel's too.
    name: "shell",
    prefix: "SHELL",
    command: "bash adk-surface/web/e2e/fixture-server.sh",
    port: 8800,
    testMatch: ["shell.spec.ts"],
    ready: "/ui/fixture.html",
  },
  {
    // The voice view (spec 3.3 §5.6): Chromium's fake microphone plays a tone recorded here, and
    // grants the microphone and the camera without asking.
    name: "voice",
    prefix: "VOICE",
    example: "scripted_voice_home",
    port: 8797,
    testMatch: ["voice.spec.ts"],
    ready: "/ui/voice",
    features: "web,voice",
    use: {
      launchOptions: {
        args: [
          "--use-fake-ui-for-media-stream",
          "--use-fake-device-for-media-stream",
          `--use-file-for-fake-audio-capture=${toneWav()}`,
        ],
      },
    },
  },
  {
    // Agents a person adds over `adk-rust up` with a scripted model (`e2e/agents-kernel.sh`),
    // whose first turn suggests one (spec 3.3 §5.7).
    name: "agents",
    prefix: "AGENTS",
    command: "bash adk-surface/web/e2e/agents-kernel.sh",
    // 8792, 8796, 8797, 8799 and 8800 are taken.
    port: 8801,
    testMatch: ["agents.spec.ts"],
    ready: "/ui/",
  },
];

/** One second of a 440 Hz tone, 16 kHz mono PCM16, for Chromium's fake microphone. */
function toneWav(): string {
  const rate = 16_000;
  const samples = new Int16Array(rate).map((_, i) => Math.round(Math.sin((2 * Math.PI * 440 * i) / rate) * 8_000));
  const header = Buffer.alloc(44);
  header.write("RIFF", 0);
  header.writeUInt32LE(36 + samples.byteLength, 4);
  header.write("WAVEfmt ", 8);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(rate, 24);
  header.writeUInt32LE(rate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write("data", 36);
  header.writeUInt32LE(samples.byteLength, 40);
  const path = join(tmpdir(), "adk-voice-e2e-tone.wav");
  writeFileSync(path, Buffer.concat([header, Buffer.from(samples.buffer)]));
  return path;
}

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
  const command =
    env(surface, "BIN") ??
    surface.command ??
    `cargo run -p adk-surface --features ${surface.features ?? "web"} --example ${surface.example} --`;
  return {
    surface,
    url: url ?? `http://${surface.host ?? "127.0.0.1"}:${port}`,
    // Readiness is checked on the address the server binds.
    ready: url ?? `http://127.0.0.1:${port}`,
    external: url !== undefined,
    port,
    workspace,
    command,
  };
});

const webServer: PlaywrightTestConfig["webServer"] = servers
  .filter((server) => !server.external)
  .map((server) => ({
    command: `${server.command} ${server.port} ${server.workspace} ${server.surface.args ?? ""}`.trim(),
    cwd: "../..",
    url: `${server.ready}${server.surface.ready}`,
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
    use: { ...server.surface.use, baseURL: server.url },
  })),
  webServer,
});
