/**
 * `@zavora-ai/adk-surface-react` (spec 3.3 §6.1 `genui`, decision 11): the renderer of the
 * web home of adk-surface — what agents draw (the `forms` 1.0 catalog and any extension
 * catalog the home registers, projected from A2UI 1.0 or v0.9.1, with no approval controls),
 * the notice of a drawing that was refused, and apps from MCP servers
 * framed sandboxed, which can never approve anything. It replaces `@zavora-ai/adk-ui-react`.
 *
 * Import `@zavora-ai/adk-surface-react/style.css` for the renderer's styles, and
 * `@zavora-ai/adk-surface-react/tokens.css` for the §9 colour tokens they read, unless the host
 * page defines its own.
 */
import "./controls.css";

export * from "../../web/src/genui/index";
export type * from "../../web/src/generated/api";
