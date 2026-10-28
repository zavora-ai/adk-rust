- **Card hints and screen actions after the wave 4 review** (`adk-surface`, `adk-cli`; spec 3.3
  §5.6, §5.7, §7.1, §11):
  - The terminal attached to `adk-rust up` draws an agent's screen action as the home does —
    "Move News digest to 19:00", "You can change it back", no **Always allow** — and lists the
    grant in an agent's settings when the kernel hosts screen actions; it read
    "Verb snooze to 2026-10-27T19:00:00+03:00" and "Can't be undone".
  - A card's "Finance read 1 web page (example.com)" counts only the pages `web_fetch`
    fetched, by host name; a refused or failed call, and the rest of what the model wrote
    into its URL, are never shown. An added or renamed agent is named as its card names it,
    the primary agent as ADK-Rust, and each request's turn is read once.
  - The limit of three waiting requests counts one person's pending requests on every page,
    and never refuses a call the person already answered, resumed or edited.
  - A cancel that arrives once a spoken snooze's window has ended no longer stops it part
    way: it runs to its end, is recorded and is reported as done.
  - A time that is not one of an item's two reads "It can only move to 19:00 or Tue 18:00";
    "It moved since you saw this" only when the item already wakes as late. Two snoozes of one
    item at once move it once.
  - An agent's record cites the approval of this very call and where the person gave it; a
    refusal names the item's title, and "What your agents did" names a yes given in Telegram
    and no longer says every refusal "had moved".
  - By voice, "snooze … tomorrow" for an item that ends first says when it ends; an ordinary
    sentence reads nothing of Coming up; a second outcome no longer hides a pending snooze's
    **Cancel**; titles are shown with reordering characters escaped.
  - The screen-action grant is listed only in Ask first and Go ahead
    (`GrantedUse::only_in`); a Telegram card a hint marks as spending money offers no answer.
