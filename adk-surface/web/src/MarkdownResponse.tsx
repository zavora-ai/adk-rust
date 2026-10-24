import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

type MarkdownResponseProps = {
  children: string;
};

/** Renders model-authored Markdown without allowing raw HTML execution. */
export function MarkdownResponse({ children }: MarkdownResponseProps) {
  return <ReactMarkdown
    remarkPlugins={[remarkGfm]}
    skipHtml
    components={{
      a: ({ children: label, href }) => <a href={href} target="_blank" rel="noreferrer">{label}</a>,
    }}
  >
    {children}
  </ReactMarkdown>;
}
