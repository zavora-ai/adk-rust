/** Parse a standards-compliant SSE byte stream, including multi-line data fields. */
export async function* parseSse(
  stream: ReadableStream<Uint8Array>,
): AsyncGenerator<{ event?: string; data: string; id?: string }> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let data: string[] = [];
  let event: string | undefined;
  let id: string | undefined;

  const dispatch = () => {
    if (!data.length) return undefined;
    const result = { event, data: data.join("\n"), id };
    data = [];
    event = undefined;
    return result;
  };

  try {
    while (true) {
      const { done, value } = await reader.read();
      buffer += decoder.decode(value, { stream: !done });
      const lines = buffer.split(/\r?\n/);
      buffer = done ? "" : (lines.pop() ?? "");
      for (const line of lines) {
        if (line === "") {
          const result = dispatch();
          if (result) yield result;
          continue;
        }
        if (line.startsWith(":")) continue;
        const colon = line.indexOf(":");
        const field = colon < 0 ? line : line.slice(0, colon);
        let valueText = colon < 0 ? "" : line.slice(colon + 1);
        if (valueText.startsWith(" ")) valueText = valueText.slice(1);
        if (field === "data") data.push(valueText);
        else if (field === "event") event = valueText;
        else if (field === "id") id = valueText;
      }
      if (done) {
        if (buffer) data.push(buffer.startsWith("data:") ? buffer.slice(5).trimStart() : buffer);
        const result = dispatch();
        if (result) yield result;
        break;
      }
    }
  } finally {
    reader.releaseLock();
  }
}
