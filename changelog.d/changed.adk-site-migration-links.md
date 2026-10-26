- **Migration drops links to unread pages and answers legacy paths 410** (`adk-site`): after
  a crawl, a link to a page the crawl could not read becomes its text
  (`DropReason::UnreadTarget`); legacy runtime paths (`/wp-includes/…`, theme and plugin
  files, `*.php`, and WordPress's `/wp-login.php` and `/xmlrpc.php`) the new site does not
  serve answer 410 Gone (spec 3.5 A-I4, S3).
- **One C2SP tile writer** (`adk-awp`, `adk-site`): `adk-site`'s site log writes its tiles
  with `adk-awp`'s `signing::log`, which gains `changed_tiles`, `TileRef`, `encode_bundle`,
  `decode_bundle`, `hash_tile` and `SigningError::Tile`; `adk-site` now depends on `adk-awp`
  (`signing`).
- **Every site kind drafts usage terms** (`adk-website`): public terms state `search=y`, and
  `train-ai=n, search=y` for news and media, so a launched site's `robots.txt` carries
  `Content-Usage` (spec 3.5 §4.4).
