//! A served site's settings from its project's configuration files (spec 3.5 §7.13, §7.14,
//! §7.17, §7.20): `email.json`, `upkeep.json`, `wallet.json` and the domain claim set the
//! options the `adk-website` binary and `adk-rust website new` serve with.

use adk_payments::wallet::{PaymentRequest, SpendCategory, WalletCaller};
use adk_website::deploy::ProjectPaths;
use adk_website::deploy::SiteOptions;
use adk_website::deploy::config::configure;
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::themes::ThemeId;
use awp_types::SiteKind;
use serde_json::json;

const DOMAIN: &str = "juniper.example";

async fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let options = ScaffoldOptions {
        name: "Juniper Bakery".into(),
        domain: DOMAIN.into(),
        kind: SiteKind::Business,
        languages: vec!["en".parse().unwrap()],
        theme: ThemeId::Paper,
        currency: "KES".into(),
    };
    scaffold(dir.path(), &options).await.unwrap();
    dir
}

fn write(dir: &std::path::Path, name: &str, value: serde_json::Value) {
    std::fs::write(dir.join(name), serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

/// Without configuration files the options stay as they were.
#[tokio::test]
async fn a_project_without_settings_leaves_the_options_alone() {
    let dir = project().await;
    let mut options = SiteOptions { owner: "owner".into(), ..SiteOptions::default() };
    let set = configure(&ProjectPaths::new(dir.path()), &mut options).await.unwrap();
    assert!(set.is_empty(), "{set:?}");
    assert!(options.email.is_none() && options.advisories.is_none());
    assert!(options.certificate.is_none() && options.wallet.is_none() && options.probe.is_none());
}

/// Every file sets its options: the email account with its password from a file, the
/// hosting job's sources, the wallet billed through the host, and the probe of the claimed
/// domain.
#[tokio::test]
async fn the_configuration_files_set_email_upkeep_wallet_and_probe() {
    let dir = project().await;
    let root = dir.path();
    std::fs::write(root.join("mail.secret"), "an app password\n").unwrap();
    write(
        root,
        "email.json",
        json!({
            "imap": { "host": "imap.mail.example", "port": 993, "security": "tls" },
            "smtp": { "host": "smtp.mail.example", "port": 587, "security": "starttls" },
            "username": "hello@juniper.example",
            "password": "file:mail.secret",
            "fromName": "Juniper Bakery",
            "senderPolicy": { "dkimSelector": "mail", "spfMechanism": "include:_spf.mail.example" }
        }),
    );
    write(
        root,
        "upkeep.json",
        json!({ "lockfile": "Cargo.lock", "advisories": "advisory-db", "certificate": true }),
    );
    write(
        root,
        "wallet.json",
        json!({
            "policy": {
                "currency": "KES",
                "period": "month",
                "categories": {
                    "hosting": {
                        "capPerPeriod": 300_000, "maxPerPayment": 300_000,
                        "payees": [{ "id": "hostco" }], "rail": "provider_billing:hostco"
                    }
                },
                "anomaly": { "multiple": 3.0, "trailingDays": 14, "minHistoryDays": 7 },
                "reconciliation": "monthly"
            },
            "billing": ["hostco"]
        }),
    );
    let claims = root.join("data/claims");
    std::fs::create_dir_all(&claims).unwrap();
    let claim = adk_site::migrate::claim::DomainClaim::new(DOMAIN, chrono::Utc::now()).unwrap();
    std::fs::write(claims.join(format!("{DOMAIN}.json")), serde_json::to_vec(&claim).unwrap())
        .unwrap();

    let paths = ProjectPaths::new(root);
    let mut options = SiteOptions { owner: "owner".into(), ..SiteOptions::default() };
    let set = configure(&paths, &mut options).await.unwrap();
    assert_eq!(
        set,
        [
            "email as hello@juniper.example",
            "advisories from advisory-db against Cargo.lock",
            "the origin's certificate",
            "the wallet, billed through hostco",
            "the Guardian's probe of the claimed domain",
        ]
    );
    let email = options.email.as_ref().unwrap();
    assert_eq!(email.from.address, "hello@juniper.example");
    assert_eq!(email.from.name.as_deref(), Some("Juniper Bakery"));
    assert!(email.domain_auth.is_some(), "bulk sends wait for SPF, DKIM and DMARC");
    let (lockfile, _) = options.advisories.as_ref().unwrap();
    assert_eq!(lockfile, &root.join("Cargo.lock"));
    assert!(options.certificate.is_some());
    assert!(options.probe.as_ref().is_some_and(|probe| probe.claim.is_none()), "the file's claim");

    // The host bills the owner's account within the mandate; the charge is recorded where
    // reconciliation finds it, and a restart keeps the payment.
    let wallet = options.wallet.clone().unwrap();
    let request = PaymentRequest::new(
        "hosting-2026-10",
        SpendCategory::Hosting,
        adk_payments::wallet::Payee::new("hostco"),
        120_000,
        "KES",
    );
    let outcome = wallet
        .pay(request, &WalletCaller::agent("urn:adk:agent:juniper-example/finance"))
        .await
        .unwrap();
    assert!(outcome.is_settled(), "{outcome:?}");
    let charges =
        std::fs::read_to_string(paths.data().join("wallet/hostco/charges.jsonl")).unwrap();
    assert_eq!(charges.lines().count(), 1);
    assert!(charges.contains("\"amount\":120000"), "{charges}");
    let mut again = SiteOptions { owner: "owner".into(), ..SiteOptions::default() };
    configure(&paths, &mut again).await.unwrap();
    let kept = again.wallet.unwrap().payment(&outcome.record().id).await.unwrap();
    assert!(kept.is_some(), "the wallet's payments are durable in data/wallet.db");
}

/// A secret written in a configuration file is refused (A-I19).
#[tokio::test]
async fn a_secret_written_in_the_file_is_refused() {
    let dir = project().await;
    write(
        dir.path(),
        "email.json",
        json!({
            "imap": { "host": "imap.mail.example", "port": 993, "security": "tls" },
            "smtp": { "host": "smtp.mail.example", "port": 465, "security": "tls" },
            "username": "hello@juniper.example",
            "password": "hunter2"
        }),
    );
    let mut options = SiteOptions::default();
    let error = configure(&ProjectPaths::new(dir.path()), &mut options).await.unwrap_err();
    assert!(error.to_string().contains("env:VAR` or `file:PATH"), "{error}");
}
