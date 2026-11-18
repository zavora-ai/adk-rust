- **A drawing refused by its catalog's schema says what broke** (`adk-surface`, feature
  `genui`). Some properties are "a binding or a literal list", a `oneOf`. When such a list
  failed, the reason quoted the whole list as "not valid under any of the schemas listed in
  the 'oneOf' keyword". The agent couldn't tell what to fix.
  - The reason now names the deepest failure among the branches, for example
    `/objects/2/motion/seconds: 336 is greater than the maximum of 120`.
  - A reason that quotes a value is shortened to 300 characters.
  - Found in a live run, where a model's solar system was refused for an orbit slower than a
    scene moves.
