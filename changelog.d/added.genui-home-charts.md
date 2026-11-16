- **Charts in the web home, and host renderers for extension components** (`adk-surface`,
  feature `web`; `@zavora-ai/adk-surface-react`): `DrawnView` draws a drawn chart natively
  with `ChartView`, a dependency-free SVG (line, area, bar, scatter, pie, sparkline) in the new
  `--chart-1` … `--chart-8`, `--chart-grid` and `--chart-axis` tokens, light and dark, kept
  apart from the brand, waiting and decision colours; its accessible name is the sentence the
  server wrote, each point names its value on hover, and it has no links, markup or animation.
  `DrawnView` takes `renderers` (`ExtensionRenderers`, by catalog id and component type): a
  renderer gets an extension component's resolved and checked properties, the nodes it holds
  and `renderChild`, and never a way to send or change a value; a component without one is
  drawn as its home view. The package exports `ChartView`, `ExtensionRendererProps` and
  `visible`. The home's embedded assets are rebuilt.
