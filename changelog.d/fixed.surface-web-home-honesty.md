- **The web home says what happened** (`adk-surface`, `adk-kernel`): a turn whose model call
  fails shows "Claims Desk couldn't answer: <reason>" in the thread where the reply would have
  been (`ThreadItem::TurnFailed`, `Source::turn_failures`, and the feed's `changed` frame for a
  thread already open); text the agent writes between tool calls of a turn reads as a note
  with its steps, and only the turn's last text as its reply; the status line lists the parts of
  the home the server serves (`StatusLine::serves`, `Served`, `SurfaceEngine::serve`,
  `Kernel::serves_routes`), and the home sends no request to a part it leaves out, so a server
  without a camera hub, the kernel's control plane, screen actions, the website, the workflow
  observer or voice logs no error and the page's console stays clean; an image no feature says
  leaves is titled "Attached image" instead of "The exact image that would leave".
