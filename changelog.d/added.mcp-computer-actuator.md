- **`McpComputerActuator` drives a desktop through computer-use-mcp's plain tools**
  (`adk-computer-use`): a `ComputerActuator` for `ComputerUseToolset` that performs each action
  with the server's `screenshot`, `click`, `mouse_move`, `scroll`, `type`, `key`, `hold_key`
  and `cursor_position` tools, plus drag and mouse down/up where its profile lists them. It
  converts the model's screenshot pixels to screen points with the mapping each screenshot
  states, and offers only the actions the server lists, so every desktop action takes the
  governed tool path. The docs note that `ComputerUseMcpRuntime` calls governed tools no
  released computer-use-mcp serves.
