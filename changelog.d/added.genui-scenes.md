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
  - **The web home** draws a scene with WebGL (`three` 0.185.1, pinned). It loads in its
    own chunk, only for a drawing that has a scene, and the main chunk is checked to stay
    free of it (`npm run check:chunks`, part of `verify`).
    - It draws in the home's palette, light and dark. Labels, ticks and panels are page
      text over the canvas.
    - The person can orbit and zoom with the pointer or the arrow keys, + and −, and Home
      to reset.
    - Picking fills the drawing's field through the home's own field state, outlines what
      is picked, and announces it.
    - Motion pauses when the scene is off screen, under reduced motion, or with Pause.
    - A patch eases objects that keep their ids to their new places, and the camera stays
      where the person left it.
    - At most six scenes draw at once; older ones fall back to their words.
    - Its words stand in where WebGL is missing or fails, and on "Show as text".
  - **Depth effects on the web** use CSS 3D:
    - a card tilts towards the pointer;
    - a flip card turns over with its own button, and its hidden face is inert;
    - a carousel turns with Previous and Next;
    - layers stack, a wall curves, and a stage turns by drag or arrow keys.
    - A field with a problem is never left on a hidden face. Under reduced motion, and on
      narrow screens, they lay out flat.
