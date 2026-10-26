- **CalDAV calendar driver** (`adk-website`): `edge::caldav::CalDavCalendar` lists the owner's
  open slots from any CalDAV server (RFC 4791), recurring slots expanded and busy events
  respected, and books each slot once with a conditional `PUT`; a served site reads it from
  `caldav.json`, secrets as `env:` or `file:` references (spec 3.5 §7.5, §7.16 `booking`).
