import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { MarkdownResponse } from "../src/MarkdownResponse";

describe("MarkdownResponse", () => {
  it("renders GFM structure", () => {
    const html = renderToStaticMarkup(
      <MarkdownResponse>{"## Result\n\n- first\n- second\n\n| A | B |\n| - | - |\n| 1 | 2 |"}</MarkdownResponse>,
    );
    expect(html).toContain("<h2>Result</h2>");
    expect(html).toContain("<li>first</li>");
    expect(html).toContain("<table>");
  });

  it("does not execute or render raw HTML", () => {
    const html = renderToStaticMarkup(
      <MarkdownResponse>{"safe <script>alert('no')</script> text"}</MarkdownResponse>,
    );
    expect(html).not.toContain("<script>");
    expect(html).toContain("alert(&#x27;no&#x27;)");
  });
});
