// Builds every server the end-to-end projects start (`playwright.config.ts`), before
// `npm run e2e` starts them. Playwright starts the servers one after another before the first
// test, so a server that compiled as it started would hold back every test and age the kernels
// started before it. `--project <name>` builds only the servers of the projects named.
import { execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { builds } from "../playwright.config.ts";

const root = fileURLToPath(new URL("../../..", import.meta.url));
for (const command of builds) {
  console.log(`> ${command}`);
  execSync(command, { cwd: root, stdio: "inherit" });
}
