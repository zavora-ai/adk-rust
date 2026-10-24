import { describe, expect, it } from "vitest";
import { parseSse } from "../src/sse";

function stream(...chunks: string[]) {
  const encoder = new TextEncoder();
  return new ReadableStream<Uint8Array>({
    start(controller) {
      chunks.forEach((chunk) => controller.enqueue(encoder.encode(chunk)));
      controller.close();
    },
  });
}

describe("parseSse", () => {
  it("parses frames split across arbitrary chunks", async () => {
    const frames = [];
    for await (const frame of parseSse(stream("id: 7\nda", "ta: {\"ok\":", "true}\n\n"))) frames.push(frame);
    expect(frames).toEqual([{ id: "7", data: "{\"ok\":true}", event: undefined }]);
  });

  it("joins multiple data fields and ignores comments", async () => {
    const frames = [];
    for await (const frame of parseSse(stream(": ping\nevent: update\ndata: first\ndata: second\n\n"))) frames.push(frame);
    expect(frames).toEqual([{ event: "update", data: "first\nsecond", id: undefined }]);
  });

  it("dispatches a final unterminated data line once", async () => {
    const frames = [];
    for await (const frame of parseSse(stream("data: final"))) frames.push(frame);
    expect(frames).toHaveLength(1);
    expect(frames[0]?.data).toBe("final");
  });
});
