- **`JwtValidator` honours every configured algorithm** (`adk-auth`, feature `sso`): a
  validator built with more than one algorithm accepted tokens signed with the first one only,
  and refused every token when the list mixed key families (RS256 with ES256). The token's
  algorithm is now checked against the configured set and validated on its own. A token signed
  with an algorithm that was not configured is still refused.
