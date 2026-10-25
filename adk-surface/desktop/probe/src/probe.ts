// The WebKitGTK probe (spec 3.3 §5.5: "Tauri 2, after a WebKitGTK test on Linux"). It checks
// what the home's later panes need from the webview — a terminal on the DOM and WebGL
// renderers with a local echo, a long unified diff in JetBrains Mono that scrolls and selects,
// and the CSS the home uses — and reports it as JSON on `window.__probe` for the WebDriver test.
import "@xterm/xterm/css/xterm.css";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import { createTwoFilesPatch } from "diff";

interface Probe {
  done: boolean;
  userAgent: string;
  rendered: { dom: boolean; webgl: boolean };
  webglContext: boolean;
  webglError?: string;
  linesPerSecond: number;
  echo: string;
  fontsLoaded: boolean;
  diff: { lines: number; scrolled: boolean; selected: number };
  colorMix: string;
  dvh: string;
  errors: string[];
}

declare global {
  interface Window {
    __probe: Probe;
  }
}

const probe: Probe = {
  done: false,
  userAgent: navigator.userAgent,
  rendered: { dom: false, webgl: false },
  webglContext: false,
  linesPerSecond: 0,
  echo: "",
  fontsLoaded: false,
  diff: { lines: 0, scrolled: false, selected: 0 },
  colorMix: "",
  dvh: "",
  errors: [],
};
window.__probe = probe;

const FONT = '"JetBrains Mono Variable", monospace';

function element(id: string): HTMLElement {
  const found = document.getElementById(id);
  if (!found) throw new Error(`#${id} is missing`);
  return found;
}

/** Resolves once `text` is parsed. */
function write(term: Terminal, text: string): Promise<void> {
  return new Promise((resolve) => term.write(text, () => resolve()));
}

/** Resolves after the next two frames, by which the renderer has drawn what was written. */
function drawn(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
}

/** The text the DOM renderer shows. */
function shown(id: string): string {
  return element(id).querySelector(".xterm-rows")?.textContent ?? "";
}

async function terminals() {
  const dom = new Terminal({ fontFamily: FONT, fontSize: 13, rows: 10, cols: 80 });
  dom.open(element("term-dom"));
  // A local echo: what is typed is written back, Enter as a new line.
  dom.onData((data) => {
    probe.echo += data;
    dom.write(data === "\r" ? "\r\n" : data);
  });
  await write(dom, "probe: DOM renderer\r\n$ ");
  await drawn();
  probe.rendered.dom = shown("term-dom").includes("probe: DOM renderer");

  const lines = 3_000;
  const started = performance.now();
  let text = "";
  for (let line = 1; line <= lines; line++) text += `line ${line} of the throughput check\r\n`;
  await write(dom, text);
  probe.linesPerSecond = Math.round(lines / ((performance.now() - started) / 1_000));
  await write(dom, "$ ");

  probe.webglContext = document.createElement("canvas").getContext("webgl2") !== null;
  const gl = new Terminal({ fontFamily: FONT, fontSize: 13, rows: 10, cols: 80 });
  gl.open(element("term-gl"));
  try {
    const addon = new WebglAddon();
    addon.onContextLoss(() => {
      probe.rendered.webgl = false;
      probe.webglError = "context lost";
      addon.dispose();
    });
    gl.loadAddon(addon);
    await write(gl, "probe: WebGL renderer\r\n");
    await drawn();
    probe.rendered.webgl = element("term-gl").querySelector("canvas") !== null;
  } catch (error) {
    probe.webglError = String(error);
  }
  dom.focus();
}

function diff() {
  const before: string[] = [];
  const after: string[] = [];
  for (let line = 1; line <= 3_000; line++) {
    const text = `fn step_${line}() -> u32 { ${line} }`;
    before.push(text);
    after.push(line % 7 === 0 ? `fn step_${line}() -> u32 { ${line} * 2 }` : text);
  }
  const patch = createTwoFilesPatch("before.rs", "after.rs", before.join("\n"), after.join("\n"), "", "", {
    context: 3_000,
  });
  const view = element("diff");
  const fragment = document.createDocumentFragment();
  let count = 0;
  for (const text of patch.split("\n")) {
    const row = document.createElement("div");
    row.textContent = text;
    if (text.startsWith("+") && !text.startsWith("+++")) row.className = "add";
    else if (text.startsWith("-") && !text.startsWith("---")) row.className = "del";
    fragment.append(row);
    count++;
  }
  view.append(fragment);
  probe.diff.lines = count;
  view.scrollTop = view.scrollHeight;
  probe.diff.scrolled = view.scrollTop > 0;
  const range = document.createRange();
  range.setStart(view.children[10], 0);
  range.setEnd(view.children[40], 0);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
  probe.diff.selected = selection?.toString().length ?? 0;
}

function css() {
  probe.colorMix = getComputedStyle(element("mix")).backgroundColor;
  probe.dvh = getComputedStyle(element("dvh")).height;
}

async function run() {
  for (const step of [terminals, diff, css]) {
    try {
      await step();
    } catch (error) {
      probe.errors.push(`${step.name}: ${String(error)}`);
    }
  }
  try {
    await document.fonts.ready;
    probe.fontsLoaded = document.fonts.check(`13px ${FONT}`);
  } catch (error) {
    probe.errors.push(`fonts: ${String(error)}`);
  }
  probe.done = true;
  element("report").textContent = JSON.stringify(probe, null, 2);
}

void run();
