- **3D scenes and depth effects in agent-drawn UI** (`adk-surface`, feature `genui`; spec
  3.3 §6.1, §7.4 rule 5). An extension component's home view can now project to
  `HomeNode::Scene` and `HomeNode::Depth`, which reach every surface as `DrawnNode.scene` and
  `DrawnNode.depth`.
  - **`DrawnScene`:** a bounded, declarative scene of shapes, lines, instanced bars and
    points, a height grid, a globe and labels, with groups, a camera, lights, axes, motion,
    panels beside objects and a text view.
  - **Colours** come from the home's palette and state tokens (`--scene-*`), never colours
    the agent picks. A scene has no textures, models, fonts, shaders or links, and every
    string in it is checked by rule 5.
  - **The server writes** a scene's summary, its panels' keys, its text view's key and its
    pick note.
  - **Limits:** `MAX_SCENES` per drawing, plus objects, instances, line points, grid size,
    texts, panels, lights, motion, group depth and bytes per scene, each a `pub const`.
  - **Labels:** a state comes with a label, and so does an object that can be picked.
  - **Picking** an object fills a `ChoicePicker` the drawing shows, with a value among its
    options ("Picking one fills Dock. Nothing is sent."). It never sends; the form's Send
    still asks.
  - **What panels and walls hold:** a scene's panels, and the children of a wall or a stage,
    hold no button or field. A text view holds no component. Scenes and depth effects don't
    nest.
  - **`DrawnDepth`:** tilt, flip, layers, carousel, wall and stage.
  - **The terminal** shows a scene as its summary, its text view or labelled objects with
    their states, and its panels; a flip card as its front and back.
  - **The web home** shows the same, in words, for now.
