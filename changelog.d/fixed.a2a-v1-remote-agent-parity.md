- **`RemoteA2aV1Agent` keeps its remote conversation and sends the whole turn**
  (`adk-server`):
  - **Conversation:** each turn used to start a new remote conversation. The agent now
    keeps the remote `contextId`, and the `taskId` of a remote task waiting on this side,
    in session state (`a2a_remote:{agent}:context_id` and `:task_id`), and continues them
    on the next turn, as adk-go does.
  - **What it sends:** it used to send only the first text part of the user's message. It
    now sends the session since its last reply: the person's messages, other agents'
    turns as context, images and files. Function calls and responses go as data parts
    tagged `adk_type`, so an answer to a remote long-running call resumes the remote task
    on any SDK.
  - **What it reads:** replies keep their images, files and untyped data, not only text.
  - **A remote waiting on its caller:** a remote `INPUT_REQUIRED` ends the turn with the
    calls it waits on as long-running function calls (`long_running_tool_ids`), for the
    caller to answer.
