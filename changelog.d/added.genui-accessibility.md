- **Accessibility as a contract** (`adk-surface`, feature `genui`): A2UI 1.0 `@accessibility`
  (label, description, `live`, `hidden`) reaches every surface resolved and checked on
  `DrawnNode.accessibility` (`DrawnAccessibility`). The web home sets it on the element (a
  labelled container is a named group), says each drawing once to a screen reader as it
  arrives, and gives every chart a **Show the data** table. A label that names nothing is a
  `genui.warned.accessibility` warning.
