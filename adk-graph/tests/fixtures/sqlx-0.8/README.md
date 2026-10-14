# SQLx 0.8 upgrade fixtures

`checkpoints_sqlite.sql` hold databases written by adk-graph 2.3.0 from crates.io, the last release on
SQLx 0.8. The `upgrade_from_sqlx_0_8` test loads them and opens them with the current
crate.

The generator program and the commands that produced every SQLx 0.8 fixture in the
workspace are in [`adk-session/tests/fixtures/sqlx-0.8/README.md`](../../../../adk-session/tests/fixtures/sqlx-0.8/README.md).
