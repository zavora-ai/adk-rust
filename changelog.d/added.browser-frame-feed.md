- **Live frame feed from a browser session** (`adk-browser`): `BrowserSession::frames(every)`
  returns a `FrameFeed` of WebDriver screenshots on an interval (at least
  `MIN_FRAME_INTERVAL`, 100 ms), for a console that shows a person what the agent sees. It
  captures only while the session runs and never starts one. It publishes a frame only when
  the page changed, skips ticks missed while a screenshot is in flight, and keeps only the
  latest frame, so a slow viewer sees the current page. It holds the session weakly and ends
  with it. It uses no remote desktop or extra port, so it suits a sealed browser.
