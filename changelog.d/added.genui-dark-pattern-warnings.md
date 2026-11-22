- **Dark-pattern warnings** (`adk-surface`, feature `genui`): a drawing whose wording pressures
  a person — false urgency, scarcity, fake social proof, confirmshaming, a countdown, or text
  that passes for the home's own controls — is shown, and the render tool's result carries A2UI
  1.0 `ValidationResult` warnings (`genui.warned.*`, severity `warning`) telling the agent what
  to change (`dark_patterns`, `ValidatedSurface::warnings`). They warn, not refuse, until their
  false positives are measured; adk-ui's sample drawings raise none.
