// The home's main chunk stays small and free of three.js: a drawing's 3D view loads only when
// it has one (spec 3.3 §6.1 `genui`, extension catalogs). Run after `vite build`.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const dir = new URL("../../assets/web/assets/", import.meta.url).pathname;
const MAIN_BUDGET = 680 * 1024;
const files = readdirSync(dir).filter((file) => file.endsWith(".js"));
const main = files.filter((file) => file.startsWith("index-"));
const scene = files.filter((file) => file.startsWith("SceneCanvas-"));
const problems = [];
if (main.length !== 1) problems.push(`expected one main chunk, found ${main.join(", ") || "none"}`);
if (scene.length !== 1) problems.push(`expected one 3D chunk, found ${scene.join(", ") || "none"}`);
for (const file of main) {
  const text = readFileSync(join(dir, file), "utf8");
  if (text.includes("WebGLRenderer")) problems.push(`${file} carries three.js; it belongs in the lazy 3D chunk`);
  const size = statSync(join(dir, file)).size;
  if (size > MAIN_BUDGET) problems.push(`${file} is ${size} bytes; the budget is ${MAIN_BUDGET}`);
}
if (problems.length) {
  console.error(problems.join("\n"));
  process.exit(1);
}
console.log(`chunks ok: ${main[0]}, ${scene[0]}`);
