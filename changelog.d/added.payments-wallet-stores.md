- **Durable wallet stores** (`adk-payments`, `sqlite`, `postgres`, `redis`):
  `SqliteWalletStore`, `PostgresWalletStore` and `RedisWalletStore` keep a site wallet's
  payments, held payments and paused categories through a restart and share them between
  instances (spec 3.5 §7.14). A key pays once across instances, and two instances over one
  store and a durable spend ledger built with `WalletPolicy::spend_limits` never settle past
  a category's cap. The SQL stores migrate additively under `_adk_wallet_migrations`; the
  Redis store keeps one hash tag per wallet and refuses a newer layout. Each feature implies
  `wallet` and the same adk-session backend. Experimental.
