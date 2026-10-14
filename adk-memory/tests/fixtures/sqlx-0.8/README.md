# SQLx 0.8 upgrade fixtures

`memory_sqlite.sql` and `memory_postgres.sql` hold databases written by adk-memory 2.3.0 from crates.io, the last release on
SQLx 0.8. The `upgrade_from_sqlx_0_8` test loads them and opens them with the current
crate.

The generator program and the commands that produced every SQLx 0.8 fixture in the
workspace are in [`adk-session/tests/fixtures/sqlx-0.8/README.md`](../../../../adk-session/tests/fixtures/sqlx-0.8/README.md).
