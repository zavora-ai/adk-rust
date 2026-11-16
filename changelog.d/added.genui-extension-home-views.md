- **Extension components in the thread and the terminal** (`adk-surface`, features `genui`,
  `web`, `tui`): a drawing in an extension catalog is projected as its components' home views,
  as the validator checked them. `DrawnNode` gains `chart` (a `DrawnChart` the home draws
  natively) and `extension` (`DrawnExtension`: the catalog, the component, its resolved and
  checked properties, and the keys of the components it holds, for a host that draws it with
  its own renderer); every other surface draws the node's one child, its home view, and the
  components it holds where that view places them, so a form inside a container keeps its
  fields for the send. The terminal prints a chart as the sentence the server wrote for it,
  then a line of blocks per series on a number or time axis, or a table of its categories cut
  at ten rows. The OpenAPI document and the generated TypeScript carry the new types.
