//! Every driver suite against the in-memory adapters (spec 3.5 §11).

use std::sync::Arc;

use adk_drivers::ads::{AdsDriver, CampaignDraft, InMemoryAds};
use adk_drivers::analytics::{
    AnalyticsDriver, InMemoryAnalytics, InMemorySearchConsole, MetricQuery, SearchConsoleDriver,
    SearchPerformance,
};
use adk_drivers::conformance::{
    AdsHarness, AnalyticsHarness, DnsHarness, InMemoryEmailHarness, InMemorySocialHarness,
    RegistrarHarness, SearchConsoleHarness, ads_driver_contract, analytics_driver_contract,
    dns_driver_contract, email_driver_contract, registrar_driver_contract,
    search_console_driver_contract, social_driver_contract,
};
use adk_drivers::dns::{
    DnsDriver, DnsRecord, InMemoryDns, InMemoryRegistrar, Money, RegistrarDriver,
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};

struct Dns;

#[async_trait]
impl DnsHarness for Dns {
    fn zone(&self) -> String {
        "example.test".into()
    }

    async fn driver(&self) -> Arc<dyn DnsDriver> {
        Arc::new(InMemoryDns::new().with_zone(
            "example.test",
            [
                DnsRecord::new("example.test", "NS", 3600, "ns1.example.test"),
                DnsRecord::new("www.example.test", "A", 300, "192.0.2.1"),
            ],
        ))
    }
}

#[tokio::test]
async fn in_memory_dns_keeps_the_contract() {
    dns_driver_contract(&Dns).await;
}

struct Registrar;

#[async_trait]
impl RegistrarHarness for Registrar {
    fn domain(&self) -> String {
        "bakery.example".into()
    }

    fn price_per_year(&self) -> Money {
        Money::new(1_400, "USD")
    }

    async fn driver(&self) -> Arc<dyn RegistrarDriver> {
        let expires = Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap();
        Arc::new(InMemoryRegistrar::new().with_domain(
            "bakery.example",
            expires,
            Money::new(1_400, "USD"),
        ))
    }
}

#[tokio::test]
async fn in_memory_registrar_keeps_the_contract() {
    registrar_driver_contract(&Registrar).await;
}

#[tokio::test]
async fn in_memory_email_keeps_the_contract() {
    email_driver_contract(&InMemoryEmailHarness).await;
}

fn day(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, d, 12, 0, 0).unwrap()
}

struct Analytics;

#[async_trait]
impl AnalyticsHarness for Analytics {
    async fn driver(&self) -> Arc<dyn AnalyticsDriver> {
        Arc::new(InMemoryAnalytics::new().with("visitors", day(5), 120.0).with(
            "visitors",
            day(6),
            80.0,
        ))
    }

    fn known(&self) -> (MetricQuery, f64) {
        (
            MetricQuery::new(
                "visitors",
                day(5) - Duration::hours(12),
                day(7) - Duration::hours(12),
            ),
            200.0,
        )
    }

    fn empty(&self) -> MetricQuery {
        MetricQuery::new("visitors", day(20), day(21))
    }
}

#[tokio::test]
async fn in_memory_analytics_keeps_the_contract() {
    analytics_driver_contract(&Analytics).await;
}

struct Search;

#[async_trait]
impl SearchConsoleHarness for Search {
    async fn driver(&self) -> Arc<dyn SearchConsoleDriver> {
        let first = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let second = NaiveDate::from_ymd_opt(2026, 10, 6).unwrap();
        Arc::new(
            InMemorySearchConsole::new()
                .with_day(first, 10.0, 100.0, 4.0)
                .with_day(second, 30.0, 300.0, 8.0),
        )
    }

    fn known(&self) -> (DateTime<Utc>, DateTime<Utc>, SearchPerformance) {
        let performance: SearchPerformance = serde_json::from_value(
            serde_json::json!({"clicks": 40.0, "impressions": 400.0, "ctr": 0.1, "position": 7.0}),
        )
        .unwrap();
        (day(5) - Duration::hours(12), day(7) - Duration::hours(12), performance)
    }

    fn empty(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        (day(20), day(21))
    }
}

#[tokio::test]
async fn in_memory_search_console_keeps_the_contract() {
    search_console_driver_contract(&Search).await;
}

#[tokio::test]
async fn in_memory_social_keeps_the_contract() {
    social_driver_contract(&InMemorySocialHarness).await;
}

struct Ads;

#[async_trait]
impl AdsHarness for Ads {
    async fn driver(&self) -> Arc<dyn AdsDriver> {
        Arc::new(InMemoryAds::new())
    }

    fn draft(&self) -> CampaignDraft {
        serde_json::from_value(serde_json::json!({
            "name": "Sourdough week",
            "adText": "Fresh sourdough every morning",
            "landingPage": "https://bakery.example/sourdough",
            "dailyBudgetMinor": 2_000,
            "bidMinor": 60,
            "currency": "USD"
        }))
        .unwrap()
    }

    fn window(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        (day(1), day(8))
    }
}

#[tokio::test]
async fn in_memory_ads_keep_the_contract() {
    ads_driver_contract(&Ads).await;
}
