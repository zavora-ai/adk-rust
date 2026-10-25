- **A decision's own address in the web home** (`adk-surface`, `web`): `/ui/decisions/<id>`
  opens the home at that decision in its agent's thread, so the tray, the phone and voice can
  send the person there for the answers only the home gives (spec 3.3 §7.2). A decided one says
  who answered; one that does not exist says so. The web client takes the surface it reads and
  acts for (`HttpClient(base, surface)`), and the end-to-end tests run one Playwright project
  per surface.
