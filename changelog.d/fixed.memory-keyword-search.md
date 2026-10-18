- **Keyword memory search matches through punctuation and returns the best matches**
  (`adk-memory`): `InMemoryMemoryService` and `RedisMemoryService` split text into runs
  of Unicode letters and digits, so `Hometown: Nairobi.` matches `hometown`; score each
  entry by the share of query words it contains (`SearchRequest::min_score` filters on
  it); and return the highest-scoring, then newest, entries instead of the first matches
  in storage order. `InMemoryMemoryService` keeps entries without words, such as an
  image, instead of dropping them; they appear in `list_recent` and match no query.
