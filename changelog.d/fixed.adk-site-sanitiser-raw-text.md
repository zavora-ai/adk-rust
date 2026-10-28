- **The sanitiser keeps no raw text from dropped elements** (`adk-site` `migrate`, spec 3.5 §7.8
  "Sanitiser"): an element among a list's own children is converted as itself, so an unlisted
  `<iframe>` there is dropped instead of its content becoming text; code (`pre`, `code`) drops an
  `<iframe>` with its content; and `noembed` and `noframes` are dropped with their content, as
  `script`, `style` and `noscript` are.
