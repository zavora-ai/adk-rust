// Records the official decoder's answer for every mask in recorded sam-3.1 streams: Meta's
// @meta-sam/parser decodes each payload, and its raster's SHA-256 and set-pixel count are
// written as the oracle `sam_codec_tests` holds the Rust codec to. Node runs only here,
// never in CI.
//
//   npm pack @meta-sam/parser && tar -xzf meta-sam-parser-*.tgz   # into its own directory
//   node sam-oracle.mjs <dir>/package oracle.json recorded/sam-3-1.*.json
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { basename, join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [pkg, output, ...recordings] = process.argv.slice(2);
const { decodeMaskToRaster } = await import(pathToFileURL(join(pkg, 'dist', 'index.js')).href);
const version = JSON.parse(readFileSync(join(pkg, 'package.json'), 'utf8')).version;
const masks = [];
for (const path of recordings) {
  const recording = JSON.parse(readFileSync(path, 'utf8'));
  let text = '';
  let done = null;
  for (const line of recording.response.split('\n')) {
    if (!line.startsWith('data: {')) continue;
    const event = JSON.parse(line.slice(6));
    if (event.type === 'response.output_text.delta') text += event.delta;
    if (event.type === 'response.output_text.done') done = event.text;
  }
  if (text === '' && done !== null) text = done;
  let index = 0;
  for (const match of text.matchAll(/(\d+)<\|box;[^|]*\|><\|mask;([^|]*)\|>/g)) {
    const data = match[2].split(';').find((f) => f.startsWith('data=')).slice(5);
    const [height, width] = data.split(',', 2).map(Number);
    const payload = data.slice(data.indexOf(',', data.indexOf(',') + 1) + 1);
    const encoding = payload.startsWith('~') ? 'lossless' : 'one_bit';
    const raster = Buffer.from(decodeMaskToRaster({ encoding, width, height, payload }));
    masks.push({
      recording: basename(path),
      index: index++,
      width,
      height,
      set: raster.reduce((n, b) => n + b, 0),
      sha256: createHash('sha256').update(raster).digest('hex'),
    });
  }
}
const oracle = { decoder: `@meta-sam/parser ${version} decodeMaskToRaster`, masks };
writeFileSync(output, JSON.stringify(oracle, null, 2) + '\n');
console.log(`${masks.length} masks decoded by @meta-sam/parser ${version}`);
