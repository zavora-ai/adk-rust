//! TLS negotiation and certificate verification of `PostgresSessionService` against a
//! server with `ssl=on` and a certificate issued by a private CA.
//!
//! | Variable | Value |
//! |----------|-------|
//! | `DATABASE_URL_TLS` | URL of the TLS server without query parameters, host `localhost` |
//! | `PG_TLS_ROOT_CERT` | Path of the CA certificate that issued the server certificate |
//!
//! The server certificate must name `localhost`, and the CA must not be in the Mozilla
//! root set. `PGSSLROOTCERT` must be unset, or it supplies the CA to every connection.
//!
//! ```bash
//! DATABASE_URL_TLS=postgres://postgres@localhost:5433/postgres PG_TLS_ROOT_CERT=ca.crt \
//!   cargo nextest run -p adk-session --features postgres --run-ignored only \
//!   --test postgres_tls_tests
//! ```

#![cfg(feature = "postgres")]

use adk_session::{CreateRequest, PostgresSessionService, SessionService};
use sqlx::PgPool;

fn tls_url(query: &str) -> String {
    let base = std::env::var("DATABASE_URL_TLS")
        .expect("set DATABASE_URL_TLS to a TLS-enabled PostgreSQL server");
    if query.is_empty() { base } else { format!("{base}?{query}") }
}

fn root_cert() -> String {
    std::env::var("PG_TLS_ROOT_CERT").expect("set PG_TLS_ROOT_CERT to the server's CA certificate")
}

/// Connects the session service and a separate pool with `url`, and reports whether
/// the pool's connection is encrypted after the service has migrated and written.
async fn connect_and_check_ssl(url: &str) -> adk_core::Result<bool> {
    adk_core::ensure_crypto_provider();
    let service = PostgresSessionService::new(url).await?;
    service.migrate().await?;
    service
        .create(CreateRequest {
            app_name: format!("tls-{}", uuid::Uuid::new_v4()),
            user_id: "user".into(),
            session_id: None,
            state: Default::default(),
        })
        .await?;
    let pool = PgPool::connect(url).await.expect("connect a pool with the same URL");
    let ssl: bool = sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .expect("query pg_stat_ssl");
    pool.close().await;
    Ok(ssl)
}

#[tokio::test]
#[ignore = "requires a TLS-enabled PostgreSQL (DATABASE_URL_TLS)"]
async fn the_default_sslmode_encrypts_when_the_server_offers_tls() {
    assert!(connect_and_check_ssl(&tls_url("")).await.expect("connect with sslmode=prefer"));
}

#[tokio::test]
#[ignore = "requires a TLS-enabled PostgreSQL (DATABASE_URL_TLS)"]
async fn sslmode_require_encrypts_without_verifying() {
    assert!(
        connect_and_check_ssl(&tls_url("sslmode=require"))
            .await
            .expect("connect with sslmode=require")
    );
}

#[tokio::test]
#[ignore = "requires a TLS-enabled PostgreSQL (DATABASE_URL_TLS)"]
async fn verify_full_accepts_the_server_with_its_root_certificate() {
    let url = tls_url(&format!("sslmode=verify-full&sslrootcert={}", root_cert()));
    assert!(connect_and_check_ssl(&url).await.expect("connect with sslmode=verify-full"));
}

#[tokio::test]
#[ignore = "requires a TLS-enabled PostgreSQL (DATABASE_URL_TLS)"]
async fn verify_full_rejects_a_private_ca_without_its_root_certificate() {
    adk_core::ensure_crypto_provider();
    let error = PostgresSessionService::new(&tls_url("sslmode=verify-full"))
        .await
        .err()
        .expect("verify-full trusted a CA outside the Mozilla root set");
    assert!(error.to_string().contains("UnknownIssuer"), "{error}");
}
