- **`ToolApproval` carries edits and messages** (`adk-core`): `ToolApproval` gains
  `edited_args` and `message` and is no longer `Copy`; construct it with `approve()` or
  `deny()` and the `with_*` setters instead of a struct literal. An approval with edited
  arguments executes the call with them after the `ToolPolicy` and tool screen check them
  again; a denial with a message reports it to the model. `ApprovalStore` adds methods with
  defaults, so existing third-party stores compile; the approval service needs them
  implemented.
