# A2UI extension: display-only surfaces (`adkrust_display_only` 1.0)

| | |
|---|---|
| **Status** | Proposed, for the A2UI project; implemented by ADK-Rust `genui` (spec 3.3 §7.4 rule 5) |
| **Extension key** | `adkrust_display_only` (a vendor key: A2UI 1.0 reserves `a2ui_` for official extensions) |
| **A2UI version** | 1.0 |
| **Date** | 2026-11-19 |

## Why

A2UI lets an agent draw any component its catalog has, and leaves to each product whether a
drawn **Approve** button, a "you said yes" banner or a fake host dialog may appear. Research on
agent UIs shows why that is not enough:

- Prompt injection can forge or reword the approval dialog itself ("Lies-in-the-Loop",
  Checkmarx 2025): if a drawing can look like a decision, an injected drawing can stand in for
  one.
- Models add pressure to generated UI unprompted: dark patterns in every generated site in one
  study (Krauß et al., CHI 2025), in more than half of generated components in another.
- People accept almost every approval prompt they are shown (93% in Anthropic's auto-mode
  report), so the prompt must be one the host owns and the agent cannot imitate.

This extension makes the separation a protocol property a renderer can check and claim: **a
drawing shows; only the host decides.**

## Claim

A surface drawn under this extension says so where A2UI 1.0 carries extension metadata, its
`createSurface` (`metadata.extensions`, a UAX #31 key outside the reserved `a2ui_` namespace):

```json
{
  "version": "1.0",
  "createSurface": {
    "surfaceId": "invoice-10",
    "catalogId": "https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json",
    "metadata": { "extensions": { "adkrust_display_only": { "version": "1.0" } } },
    "components": [ … ]
  }
}
```

A renderer that applies the rules may list the key in its own claims; ADK-Rust's home lists it
in `GET /api/surface/genui/capabilities`. A renderer that does not know the key ignores it, as
A2UI 1.0 requires, and the surface still renders.

## Rules

A surface under this extension MUST follow every rule below. An agent SHOULD check a drawing
before sending it; a renderer MUST check it again before drawing it, and MUST refuse, not
repair, a drawing that breaks one.

1. **No decision's words.** No string a person reads or hears — text, labels, placeholders,
   options, accessibility labels and descriptions, bound data, `formatString` and `pluralize`
   results, and a server-written summary — contains a decision's phrases ("Allow once",
   "Always allow", "You said yes", "Confirm it's you" and their translations), after folding
   case, look-alike and zero-width characters, nor is a decision's single word ("Approve",
   "Deny", "Done") as a whole label.
2. **No secrets.** No field asks for a password or another secret (`obscured` text fields, or
   fields whose label or bound path names a secret).
3. **No links.** No string is a link, however disguised; the renderer opens nothing.
4. **Only declared actions.** A button dispatches only an event named after an action the
   drawing declares beside its components; a component that is not a button dispatches
   nothing; an event carries no `context` (the declared action binds the fields) and no
   `userMessage` (a drawing never speaks as the person).
5. **A send is a request, never a decision.** Pressing a declared action asks the host for the
   call it names, with the fields it binds. The host holds that call for the person's
   decision on a card the host draws (below); the drawing's own press is never the approval.
6. **Nothing hidden that is sent.** A field or button a send carries is never
   `@accessibility.hidden`, and every component a container holds is shown exactly once.
7. **Show-only extensions.** A component an extension catalog adds carries no action, event or
   check function, and every string and key of its properties and of the data it binds is
   checked as above.
8. **No whole-model echo.** A surface never sets `sendDataModel`: a send carries only the
   fields its card shows.

A renderer MAY additionally warn the agent (A2UI 1.0 `ValidationResult`, severity `warning`)
about wording that pressures a person: false urgency, scarcity, fake social proof,
confirmshaming, countdowns, and text imitating host controls.

## The host's approval card: a minimum content model

The decision a send asks for is drawn by the host, never by the agent. It MUST show, in the
host's own typography and without rendering markdown:

| Shown | Why |
|---|---|
| The tool's canonical name | What runs |
| Every argument the call runs with, in full (expandable past a length cap; after an edit, as a diff) | What it runs with |
| The agent that asks, and that it came from a drawn form (with the time the person pressed Send) | Who asks, and why the person is seeing this |
| What can and cannot be undone, from the tool's declared effect | Consequences |
| The answers the policy allows — never "Always allow" for a drawn form's call | Scope of the yes |

and MUST NOT show agent-supplied rationale above the call, or let any line push the call and
its answers out of sight (cap every line). The yes SHOULD be sealed: bound, with a key only
the host holds, to the tool, a hash of the arguments as approved, a hash of what the card
showed, the drawing's digest and version, the approver and an expiry, and checked right before
the call runs (ADK-Rust: `ApprovalSeals`, `RequestedCallSeal`).

## Conformance

ADK-Rust's `genui` test suites are the reference: `genui_validator_tests`,
`genui_tools_tests`, `genui_scenario_tests` (a drawn form's call held, sealed and run once),
`genui_ag_ui_tests` (the same over AG-UI 1.0 interrupts), and the A2UI 1.0 catalog documents
`GET /api/surface/genui/catalogs?version=1.0` serves, which pass the A2UI 1.0 meta-schema.

## Watch list

Not built, and why:

| | Status | What would change here |
|---|---|---|
| WebMCP (`document.modelContext`) | W3C Community Group draft, Chrome Canary only | Map its `consequentialHint` to the card's severity once it ships |
| A2A agent cards advertising A2UI catalogs | Discussed in the A2A extension | Claim this extension in the agent card |
| MCP Apps permission model and app-call provenance (`initiator: "app"`) | Deferred in the MCP Apps draft (issue #738) | `genui-apps` already records `initiator: "app"`; follow the spec when it lands |
| WebXR and display glasses | Glasses without displays ship 2026-12; displays not before 2027 | Scenes keep their text view; nothing to draw on yet |
| Gaussian splats and world models | Need assets fetched from a host | Conflict with `connect-src 'none'`; stay out of drawings |
