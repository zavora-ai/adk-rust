// Shared by the website end-to-end tests (spec 3.5 §8.2): screenshots of each screen in light
// and dark under `docs/official_docs/images/surfaces/website/` when WEBSITE_E2E_SCREENSHOTS is
// set, and a small PNG for uploads.
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Page } from "@playwright/test";

const SHOTS = process.env.WEBSITE_E2E_SCREENSHOTS
  ? fileURLToPath(new URL("../../../docs/official_docs/images/surfaces/website/", import.meta.url))
  : undefined;

/** Captures the screen in light and dark, the whole section in view. */
export async function shoot(page: Page, name: string) {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  const height = await page.evaluate(() => {
    const content = document.querySelector(".content");
    const header = document.querySelector(".header");
    return Math.ceil((content?.scrollHeight ?? 900) + (header?.getBoundingClientRect().height ?? 70));
  });
  const size = page.viewportSize() ?? { width: 1440, height: 900 };
  await page.setViewportSize({ width: 1440, height: Math.min(Math.max(height, 900), 4200) });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    await page.screenshot({ path: join(SHOTS, `${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: "light" });
  await page.setViewportSize(size);
}

/** An 8×8 PNG, stored without compression. */
export function png(): Buffer {
  const width = 8;
  const height = 8;
  const raw = Buffer.concat(Array.from({ length: height }, () => Buffer.from([0, ...Array(width).fill([60, 40, 30]).flat()])));
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (data: Buffer) => {
    let c = 0xffffffff;
    for (const byte of data) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (kind: string, data: Buffer) => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(kind, "ascii"), data]);
    const sum = Buffer.alloc(4);
    sum.writeUInt32BE(crc(body));
    return Buffer.concat([length, body, sum]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 2, 0, 0, 0], 8);
  const stored = Buffer.concat([Buffer.from([0x78, 0x01, 0x01]), Buffer.from([raw.length & 0xff, raw.length >> 8, ~raw.length & 0xff, (~raw.length >> 8) & 0xff]), raw]);
  let a = 1;
  let b = 0;
  for (const byte of raw) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  const adler = Buffer.alloc(4);
  adler.writeUInt32BE(((b << 16) | a) >>> 0);
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", header), chunk("IDAT", Buffer.concat([stored, adler])), chunk("IEND", Buffer.alloc(0))]);
}
