- **`RedisSessionService` runs Lua scripts** (`adk-session`): `create` and `append_event` write
  through `EVAL` scripts, so a Redis server or ACL user that refuses scripting fails them where
  2.3.0 succeeded. Allow `EVAL` for the session service's user.
