//! The edge's commerce, kept durable (spec 3.5 §7.5 "Commerce", §7.9, §7.16 `quote`, `order`,
//! `booking`, A-I7, A-I25): the quotes the site issued, the orders placed against them and
//! the calendar slots taken survive a restart, so a buyer's quote keeps its price and an order
//! is paid once.
//!
//! | Part | What it does |
//! |---|---|
//! | [`CommerceStore`] | Quotes, orders with a compare-and-set on their status, and bookings that take a slot once |
//! | [`InMemoryCommerceStore`] | The store in memory, for tests |
//! | [`SqliteCommerceStore`] | The store in SQLite: a served site's `data/edge.db` |
//! | [`StoredCalendar`] | The slots of `calendar.json`, booked in a [`CommerceStore`] |
//! | [`conformance`] | The check every [`CommerceStore`] passes |
//!
//! The edge's [`QuoteBook`](super::handlers::QuoteBook) and
//! [`OrderBook`](super::handlers::OrderBook) read and write through a store; the payment
//! handler moves an order from `awaiting_payment` to `paying` and on to `paid` with the
//! store's compare-and-set, so two payments for one order never both start, in one process or
//! several.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{AdkError, Result};
use async_trait::async_trait;
use awp_types::QuoteDocument;
use chrono::{DateTime, Utc};
use sqlx::Row as _;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::handlers::{Calendar, Order, Slot};

fn store_error(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("the site's commerce store: {error}"))
}

/// Where the edge keeps its quotes, orders and bookings (spec 3.5 §7.5, §7.9).
///
/// # Example
///
/// ```rust
/// use adk_website::edge::commerce::{CommerceStore, InMemoryCommerceStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let store = InMemoryCommerceStore::default();
/// assert!(store.book("slot-1", "key-a").await?);
/// assert!(store.book("slot-1", "key-a").await?, "the same booking again");
/// assert!(!store.book("slot-1", "key-b").await?, "another booking finds it taken");
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait CommerceStore: Send + Sync {
    /// Keeps `quote`, replacing one with its id.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn put_quote(&self, quote: &QuoteDocument) -> Result<()>;

    /// The quote `id`.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn quote(&self, id: Uuid) -> Result<Option<QuoteDocument>>;

    /// Keeps `order` unless one with its id exists; returns the order kept.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn place_order(&self, order: &Order) -> Result<Order>;

    /// The order `id`.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn order(&self, id: &str) -> Result<Option<Order>>;

    /// Moves order `id` to `status`; `None` when there is no such order.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn set_order_status(&self, id: &str, status: &str) -> Result<Option<Order>>;

    /// Moves order `id` from `from` to `to` in one atomic step; `None` when there is no such
    /// order or it is not in `from`.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn transition_order(&self, id: &str, from: &str, to: &str) -> Result<Option<Order>>;

    /// Takes slot `slot` for booking `key`: `true` when it is taken now or was already taken
    /// by `key`, `false` when another booking holds it.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn book(&self, slot: &str, key: &str) -> Result<bool>;

    /// The slots taken, each with the booking that holds it.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn bookings(&self) -> Result<BTreeMap<String, String>>;
}

/// A [`CommerceStore`] in memory, for tests and sandboxes.
#[derive(Debug, Default)]
pub struct InMemoryCommerceStore {
    quotes: Mutex<BTreeMap<Uuid, QuoteDocument>>,
    orders: Mutex<BTreeMap<String, Order>>,
    bookings: Mutex<BTreeMap<String, String>>,
}

#[async_trait]
impl CommerceStore for InMemoryCommerceStore {
    async fn put_quote(&self, quote: &QuoteDocument) -> Result<()> {
        self.quotes.lock().await.insert(quote.id, quote.clone());
        Ok(())
    }

    async fn quote(&self, id: Uuid) -> Result<Option<QuoteDocument>> {
        Ok(self.quotes.lock().await.get(&id).cloned())
    }

    async fn place_order(&self, order: &Order) -> Result<Order> {
        Ok(self
            .orders
            .lock()
            .await
            .entry(order.id.clone())
            .or_insert_with(|| order.clone())
            .clone())
    }

    async fn order(&self, id: &str) -> Result<Option<Order>> {
        Ok(self.orders.lock().await.get(id).cloned())
    }

    async fn set_order_status(&self, id: &str, status: &str) -> Result<Option<Order>> {
        let mut orders = self.orders.lock().await;
        let Some(order) = orders.get_mut(id) else { return Ok(None) };
        order.status = status.to_string();
        Ok(Some(order.clone()))
    }

    async fn transition_order(&self, id: &str, from: &str, to: &str) -> Result<Option<Order>> {
        let mut orders = self.orders.lock().await;
        let Some(order) = orders.get_mut(id).filter(|order| order.status == from) else {
            return Ok(None);
        };
        order.status = to.to_string();
        Ok(Some(order.clone()))
    }

    async fn book(&self, slot: &str, key: &str) -> Result<bool> {
        let mut bookings = self.bookings.lock().await;
        Ok(bookings.entry(slot.to_string()).or_insert_with(|| key.to_string()) == key)
    }

    async fn bookings(&self) -> Result<BTreeMap<String, String>> {
        Ok(self.bookings.lock().await.clone())
    }
}

/// A [`CommerceStore`] in SQLite, such as a served site's `data/edge.db` (spec 3.5 §7.9).
///
/// One site's records sit under its namespace, so one database holds several sites. Call
/// [`migrate`](Self::migrate) once.
///
/// # Example
///
/// ```rust
/// use adk_website::edge::commerce::{CommerceStore, SqliteCommerceStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
/// let store = SqliteCommerceStore::new(pool).with_site("bakery");
/// store.migrate().await?;
/// assert!(store.order("order-1").await?.is_none());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteCommerceStore {
    pool: sqlx::SqlitePool,
    site: String,
}

impl SqliteCommerceStore {
    /// The store over `pool`, in the `default` namespace.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool, site: "default".into() }
    }

    /// Keeps the records under `site`.
    #[must_use]
    pub fn with_site(mut self, site: impl Into<String>) -> Self {
        self.site = site.into();
        self
    }

    /// Creates the tables.
    ///
    /// # Errors
    ///
    /// The database's error.
    pub async fn migrate(&self) -> Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS website_quotes (
                site TEXT NOT NULL, id TEXT NOT NULL, quote TEXT NOT NULL,
                PRIMARY KEY (site, id))",
            "CREATE TABLE IF NOT EXISTS website_orders (
                site TEXT NOT NULL, id TEXT NOT NULL, status TEXT NOT NULL,
                placed_at TEXT NOT NULL, record TEXT NOT NULL, PRIMARY KEY (site, id))",
            "CREATE TABLE IF NOT EXISTS website_bookings (
                site TEXT NOT NULL, slot TEXT NOT NULL, key TEXT NOT NULL, at TEXT NOT NULL,
                PRIMARY KEY (site, slot))",
        ] {
            sqlx::query(statement).execute(&self.pool).await.map_err(store_error)?;
        }
        Ok(())
    }

    fn encode(order: &Order) -> Result<String> {
        serde_json::to_string(order).map_err(store_error)
    }
}

#[async_trait]
impl CommerceStore for SqliteCommerceStore {
    async fn put_quote(&self, quote: &QuoteDocument) -> Result<()> {
        sqlx::query(
            "INSERT INTO website_quotes (site, id, quote) VALUES (?, ?, ?)
             ON CONFLICT (site, id) DO UPDATE SET quote = excluded.quote",
        )
        .bind(&self.site)
        .bind(quote.id.to_string())
        .bind(serde_json::to_string(quote).map_err(store_error)?)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn quote(&self, id: Uuid) -> Result<Option<QuoteDocument>> {
        let row = sqlx::query("SELECT quote FROM website_quotes WHERE site = ? AND id = ?")
            .bind(&self.site)
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(store_error)?;
        row.map(|row| {
            let text: String = row.try_get("quote").map_err(store_error)?;
            serde_json::from_str(&text).map_err(store_error)
        })
        .transpose()
    }

    async fn place_order(&self, order: &Order) -> Result<Order> {
        sqlx::query(
            "INSERT INTO website_orders (site, id, status, placed_at, record) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (site, id) DO NOTHING",
        )
        .bind(&self.site)
        .bind(&order.id)
        .bind(&order.status)
        .bind(order.placed_at.to_rfc3339())
        .bind(Self::encode(order)?)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        self.order(&order.id)
            .await?
            .ok_or_else(|| store_error(format!("order {} was not kept", order.id)))
    }

    async fn order(&self, id: &str) -> Result<Option<Order>> {
        let row = sqlx::query("SELECT record FROM website_orders WHERE site = ? AND id = ?")
            .bind(&self.site)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(store_error)?;
        row.map(|row| {
            let text: String = row.try_get("record").map_err(store_error)?;
            serde_json::from_str(&text).map_err(store_error)
        })
        .transpose()
    }

    async fn set_order_status(&self, id: &str, status: &str) -> Result<Option<Order>> {
        let Some(mut order) = self.order(id).await? else { return Ok(None) };
        order.status = status.to_string();
        sqlx::query("UPDATE website_orders SET status = ?, record = ? WHERE site = ? AND id = ?")
            .bind(status)
            .bind(Self::encode(&order)?)
            .bind(&self.site)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(Some(order))
    }

    async fn transition_order(&self, id: &str, from: &str, to: &str) -> Result<Option<Order>> {
        let Some(mut order) = self.order(id).await?.filter(|order| order.status == from) else {
            return Ok(None);
        };
        order.status = to.to_string();
        // The status is compared again in the update, so of two concurrent transitions out of
        // `from` only one moves the order.
        let done = sqlx::query(
            "UPDATE website_orders SET status = ?, record = ?
             WHERE site = ? AND id = ? AND status = ?",
        )
        .bind(to)
        .bind(Self::encode(&order)?)
        .bind(&self.site)
        .bind(id)
        .bind(from)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok((done.rows_affected() == 1).then_some(order))
    }

    async fn book(&self, slot: &str, key: &str) -> Result<bool> {
        sqlx::query(
            "INSERT INTO website_bookings (site, slot, key, at) VALUES (?, ?, ?, ?)
             ON CONFLICT (site, slot) DO NOTHING",
        )
        .bind(&self.site)
        .bind(slot)
        .bind(key)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        let holder: String =
            sqlx::query("SELECT key FROM website_bookings WHERE site = ? AND slot = ?")
                .bind(&self.site)
                .bind(slot)
                .fetch_one(&self.pool)
                .await
                .map_err(store_error)?
                .try_get("key")
                .map_err(store_error)?;
        Ok(holder == key)
    }

    async fn bookings(&self) -> Result<BTreeMap<String, String>> {
        let rows = sqlx::query("SELECT slot, key FROM website_bookings WHERE site = ?")
            .bind(&self.site)
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get::<String, _>("slot").map_err(store_error)?,
                    row.try_get::<String, _>("key").map_err(store_error)?,
                ))
            })
            .collect()
    }
}

/// The slots a site offers, booked in a [`CommerceStore`] (spec 3.5 §7.16 `booking`): the
/// served site's calendar for the slots of `calendar.json`, so a booking survives a restart.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_website::edge::commerce::{InMemoryCommerceStore, StoredCalendar};
/// use adk_website::edge::handlers::{Calendar, Slot};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let at = chrono::Utc::now();
/// let slot = Slot { id: "s1".into(), starts_at: at, minutes: 30, price: Some(2_000) };
/// let calendar = StoredCalendar::new([slot], Arc::new(InMemoryCommerceStore::default()));
/// assert!(calendar.take("s1", "booking-1").await?);
/// assert!(calendar.open(at, at + chrono::Duration::hours(1)).await?.is_empty());
/// assert_eq!(calendar.price("s1"), Some(2_000));
/// # Ok(())
/// # }
/// ```
pub struct StoredCalendar {
    slots: BTreeMap<String, Slot>,
    store: Arc<dyn CommerceStore>,
}

impl StoredCalendar {
    /// `slots`, booked in `store`.
    pub fn new(slots: impl IntoIterator<Item = Slot>, store: Arc<dyn CommerceStore>) -> Self {
        Self { slots: slots.into_iter().map(|slot| (slot.id.clone(), slot)).collect(), store }
    }
}

#[async_trait]
impl Calendar for StoredCalendar {
    async fn open(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Slot>> {
        let taken = self.store.bookings().await?;
        Ok(self
            .slots
            .values()
            .filter(|slot| {
                !taken.contains_key(&slot.id) && slot.starts_at >= from && slot.starts_at < to
            })
            .cloned()
            .collect())
    }

    async fn take(&self, id: &str, key: &str) -> Result<bool> {
        if !self.slots.contains_key(id) {
            return Ok(false);
        }
        self.store.book(id, key).await
    }

    fn price(&self, id: &str) -> Option<u64> {
        self.slots.get(id)?.price
    }
}

/// The conformance check every [`CommerceStore`] passes (spec 3.5 §7.5, §7.9).
pub mod conformance {
    use std::sync::Arc;

    use awp_types::{LineItem, QuoteDocument};
    use chrono::Utc;
    use uuid::Uuid;

    use super::CommerceStore;
    use crate::edge::handlers::{AWAITING_PAYMENT, Order, PAID, PAYING};

    fn quote(total: u64) -> QuoteDocument {
        let now = Utc::now();
        QuoteDocument {
            id: Uuid::new_v4(),
            capability: "get_quote".into(),
            requester: None,
            lines: vec![LineItem {
                sku: "loaf".into(),
                description: Some("Sourdough".into()),
                quantity: 2,
                unit_amount: total / 2,
            }],
            total,
            currency: "KES".into(),
            release_seq: 3,
            issued_at: now,
            valid_until: now + chrono::Duration::days(7),
            proof: None,
        }
    }

    /// Quotes read back as kept; an order is placed once, its status moves by
    /// compare-and-set, and a slot is booked once (spec 3.5 §7.5, A-I7, A-I25). `open` gives
    /// a new handle over the same data, as a restarted process would.
    ///
    /// # Panics
    ///
    /// When the store breaks its contract.
    pub async fn commerce_store<F, Fut>(open: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Arc<dyn CommerceStore>>,
    {
        let issued = quote(800);
        open().await.put_quote(&issued).await.unwrap();
        assert_eq!(open().await.quote(issued.id).await.unwrap(), Some(issued.clone()));
        assert_eq!(open().await.quote(Uuid::new_v4()).await.unwrap(), None);

        let order = Order {
            id: format!("order-{}", Uuid::new_v4().simple()),
            quote: issued.clone(),
            status: AWAITING_PAYMENT.into(),
            placed_at: Utc::now(),
        };
        assert_eq!(open().await.place_order(&order).await.unwrap(), order);
        let again = Order { status: PAID.into(), ..order.clone() };
        assert_eq!(
            open().await.place_order(&again).await.unwrap(),
            order,
            "a second placement keeps the first order"
        );
        assert_eq!(open().await.order(&order.id).await.unwrap(), Some(order.clone()));
        assert_eq!(open().await.order("order-none").await.unwrap(), None);

        let paying = open().await.transition_order(&order.id, AWAITING_PAYMENT, PAYING).await;
        assert_eq!(paying.unwrap().map(|order| order.status), Some(PAYING.to_string()));
        assert_eq!(
            open().await.transition_order(&order.id, AWAITING_PAYMENT, PAYING).await.unwrap(),
            None,
            "an order not in `from` does not move"
        );
        assert_eq!(open().await.order(&order.id).await.unwrap().unwrap().status, PAYING);
        let paid = open().await.set_order_status(&order.id, PAID).await.unwrap();
        assert_eq!(paid.map(|order| order.status), Some(PAID.to_string()));
        assert_eq!(open().await.set_order_status("order-none", PAID).await.unwrap(), None);

        // Concurrent transitions out of one status: exactly one moves the order.
        let contested = Order { id: format!("order-{}", Uuid::new_v4().simple()), ..order.clone() };
        let contested = Order { status: AWAITING_PAYMENT.into(), ..contested };
        open().await.place_order(&contested).await.unwrap();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let store = open().await;
            let id = contested.id.clone();
            tasks.push(tokio::spawn(async move {
                store.transition_order(&id, AWAITING_PAYMENT, PAYING).await.unwrap().is_some()
            }));
        }
        let mut moved = 0;
        for task in tasks {
            moved += usize::from(task.await.unwrap());
        }
        assert_eq!(moved, 1);

        let slot = format!("slot-{}", Uuid::new_v4().simple());
        assert!(open().await.book(&slot, "key-a").await.unwrap());
        assert!(open().await.book(&slot, "key-a").await.unwrap(), "the same booking again");
        assert!(!open().await.book(&slot, "key-b").await.unwrap(), "taken by another");
        assert_eq!(
            open().await.bookings().await.unwrap().get(&slot).map(String::as_str),
            Some("key-a")
        );
    }
}
