//! A served site's settings from its project's configuration files (spec 3.5 §7.13, §7.14,
//! §7.17, §7.20): what the `adk-website` binary and `adk-rust website new` read into
//! [`SiteOptions`] with [`configure`].
//!
//! | File | Sets | Holds |
//! |---|---|---|
//! | `email.json` | [`SiteOptions::email`] | The site's mailbox over IMAP and SMTP ([`EmailConfig`]): the Inbox reads it, nurture and the outbox send from it |
//! | `upkeep.json` | [`SiteOptions::advisories`], [`SiteOptions::certificate`] | The edge's `Cargo.lock` with a RustSec advisory database checkout, and whether the origin's certificate is read ([`UpkeepConfig`]) |
//! | `wallet.json` | [`SiteOptions::wallet`] | The owner's wallet policy and the providers that bill the owner's own accounts ([`WalletConfig`]) |
//! | `data/claims/{domain}.json` | [`SiteOptions::probe`] | The owner's domain claim, which `adk-rust site migrate` keeps; the Guardian proves it again with the system's resolver on every probe |
//!
//! Secrets are `env:VAR` or `file:PATH`, a path relative to the project, never the secret
//! itself (A-I19). A file that is absent leaves its options as they were.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_drivers::email::{ImapSmtpEmail, MailEndpoint, Mailbox, SenderPolicy, SystemTxtLookup};
use adk_payments::wallet::{
    BillingAccount, Debit, DebitRequest, Funding, ProviderBillingRail, RailPayment, StatementEntry,
    Wallet, WalletPolicy, WalletProvider,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::project::{ProjectPaths, read_json, secret_in};
use super::site::{EmailOptions, ProbeOptions, SiteOptions};

fn config(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(error.to_string())
}

/// How a mail server connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailSecurityConfig {
    /// TLS from the start, as on ports 465 and 993.
    Tls,
    /// Upgraded with STARTTLS, as on port 587.
    Starttls,
    /// No TLS: loopback servers only.
    Plain,
}

/// One mail server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailServerConfig {
    /// The host name, which TLS verifies.
    pub host: String,
    /// The port.
    pub port: u16,
    /// How the connection is secured.
    pub security: MailSecurityConfig,
}

impl MailServerConfig {
    fn endpoint(&self) -> Result<MailEndpoint> {
        match self.security {
            MailSecurityConfig::Tls => MailEndpoint::tls(&self.host, self.port),
            MailSecurityConfig::Starttls => MailEndpoint::starttls(&self.host, self.port),
            MailSecurityConfig::Plain => MailEndpoint::plain(&self.host, self.port),
        }
        .map_err(config)
    }
}

/// The sender policy SPF, DKIM and DMARC are checked against before bulk sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SenderPolicyConfig {
    /// The DKIM selector the sending service signs with.
    pub dkim_selector: String,
    /// The SPF mechanism that authorises the sending service, such as
    /// `include:_spf.mail.example`.
    pub spf_mechanism: String,
}

/// The site's email in `email.json` (spec 3.5 §7.13).
///
/// ```json
/// {
///   "imap": { "host": "imap.mail.example", "port": 993, "security": "tls" },
///   "smtp": { "host": "smtp.mail.example", "port": 587, "security": "starttls" },
///   "username": "hello@juniper.example",
///   "password": "env:JUNIPER_MAIL_PASSWORD",
///   "fromName": "Juniper Bakery",
///   "senderPolicy": { "dkimSelector": "mail", "spfMechanism": "include:_spf.mail.example" }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmailConfig {
    /// Where the inbox is read.
    pub imap: MailServerConfig,
    /// Where mail is sent.
    pub smtp: MailServerConfig,
    /// The account.
    pub username: String,
    /// Its password, as `env:VAR` or `file:PATH`.
    pub password: String,
    /// The address the site sends from; the account's when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The display name it sends as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    /// The mailbox read; `INBOX` when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<String>,
    /// The sender policy bulk sends wait for, checked through the system's resolver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_policy: Option<SenderPolicyConfig>,
}

/// The hosting job's sources in `upkeep.json` (spec 3.5 §7.20, S10).
///
/// ```json
/// { "lockfile": "Cargo.lock", "advisories": "/var/lib/advisory-db", "certificate": true }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpkeepConfig {
    /// The edge's `Cargo.lock`, relative to the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lockfile: Option<PathBuf>,
    /// A checkout of the RustSec advisory database (`github.com/rustsec/advisory-db`),
    /// relative to the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisories: Option<PathBuf>,
    /// Whether the certificate the site's origin presents is read for renewal proposals.
    #[serde(default)]
    pub certificate: bool,
}

/// The owner's wallet in `wallet.json` (spec 3.5 §7.14).
///
/// The policy's mandates name their rails; each provider in `billing` is a
/// `provider_billing:{provider}` rail that bills the owner's own account at that provider:
/// the wallet authorises each charge within its mandate and cap and records it in
/// `data/wallet/{provider}/charges.jsonl`, and reconciliation reads the provider's invoice
/// lines the owner exports to `data/wallet/{provider}/invoice.jsonl`. The wallet holds no
/// funds of its own here: a payment on a wallet-funded rail is refused.
///
/// ```json
/// {
///   "policy": {
///     "currency": "KES",
///     "period": "month",
///     "categories": {
///       "hosting": { "capPerPeriod": 300000, "maxPerPayment": 300000,
///                    "payees": [{ "id": "hostco" }], "rail": "provider_billing:hostco" }
///     },
///     "anomaly": { "multiple": 3.0, "trailingDays": 30, "minHistoryDays": 7, "minAmount": 0 },
///     "reconciliation": "monthly"
///   },
///   "billing": ["hostco"]
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletConfig {
    /// The owner's policy: mandates per category, caps, payees and rails.
    pub policy: WalletPolicy,
    /// The providers that bill the owner's own accounts.
    #[serde(default)]
    pub billing: Vec<String>,
}

/// A provider account that bills the owner directly, kept in files (spec 3.5 §7.14): each
/// charge the wallet allowed is appended to `charges.jsonl`, and the provider's invoice is
/// the `invoice.jsonl` the owner exports from the provider.
#[derive(Debug, Clone)]
pub struct FileBillingAccount {
    provider: String,
    dir: PathBuf,
}

impl FileBillingAccount {
    /// The account at `provider`, its files in `dir`.
    pub fn new(provider: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self { provider: provider.into(), dir: dir.into() }
    }

    fn lines(path: &Path) -> Result<Vec<StatementEntry>> {
        match std::fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    serde_json::from_str(line)
                        .map_err(|e| config(format!("{}: {e}", path.display())))
                })
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(config(format!("{}: {error}", path.display()))),
        }
    }
}

#[async_trait]
impl BillingAccount for FileBillingAccount {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn charge(&self, payment: &RailPayment) -> Result<String> {
        use std::io::Write as _;
        let path = self.dir.join("charges.jsonl");
        let reference = format!("{}-{}", self.provider, payment.payment_id);
        if Self::lines(&path)?.iter().any(|line| line.id == reference) {
            return Ok(reference);
        }
        let entry = StatementEntry {
            id: reference.clone(),
            reference: Some(payment.payment_id.clone()),
            kind: adk_payments::wallet::StatementKind::Debit,
            amount: payment.amount,
            currency: payment.currency.clone(),
            payee: Some(payment.payee.id.clone()),
            at: Utc::now(),
        };
        std::fs::create_dir_all(&self.dir).map_err(config)?;
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(config)?;
        writeln!(file, "{}", serde_json::to_string(&entry).map_err(config)?).map_err(config)?;
        Ok(reference)
    }

    async fn invoice(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<StatementEntry>> {
        Ok(Self::lines(&self.dir.join("invoice.jsonl"))?
            .into_iter()
            .filter(|line| from <= line.at && line.at < to)
            .collect())
    }
}

/// The funding of a wallet whose rails all bill the owner's accounts: it holds nothing, so a
/// payment on a wallet-funded rail is refused until the owner gives the wallet a provider.
#[derive(Debug, Clone)]
pub struct NoStoredValue {
    currency: String,
}

impl NoStoredValue {
    /// No funds in `currency`.
    pub fn new(currency: impl Into<String>) -> Self {
        Self { currency: currency.into().to_ascii_uppercase() }
    }
}

#[async_trait]
impl WalletProvider for NoStoredValue {
    fn name(&self) -> &str {
        "none"
    }

    fn funding(&self) -> Funding {
        Funding::StoredValue
    }

    fn currency(&self) -> &str {
        &self.currency
    }

    async fn balance(&self) -> Result<u64> {
        Ok(0)
    }

    async fn debit(&self, request: &DebitRequest) -> Result<Debit> {
        Err(AdkError::new(
            ErrorComponent::Guardrail,
            ErrorCategory::ResourceExhausted,
            adk_payments::wallet::WALLET_INSUFFICIENT_FUNDS_CODE,
            format!(
                "the wallet holds no funds, so it cannot pay {} {}; payments go through the \
                 providers that bill the owner's accounts",
                request.amount, request.currency
            ),
        ))
    }

    async fn void(&self, _debit: &Debit) -> Result<()> {
        Ok(())
    }

    async fn statement(
        &self,
        _from: DateTime<Utc>,
        _to: DateTime<Utc>,
    ) -> Result<Vec<StatementEntry>> {
        Ok(Vec::new())
    }
}

/// Sets `options` from the configuration files in `paths` (spec 3.5 §7.17): the email, the
/// upkeep sources, the wallet and the Guardian's probe. Returns what it set, in words, for
/// the person starting the site.
///
/// # Example
///
/// ```rust
/// use adk_website::deploy::{ProjectPaths, SiteOptions, config::configure};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("upkeep.json"), r#"{ "lockfile": "Cargo.lock", "advisories": "advisory-db" }"#).unwrap();
/// let mut options = SiteOptions::default();
/// let set = configure(&ProjectPaths::new(dir.path()), &mut options).await?;
/// assert!(options.advisories.is_some());
/// assert_eq!(set, ["advisories from advisory-db against Cargo.lock"]);
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// A file that does not parse, a secret written in a file or not found, a mail server or
/// policy the drivers refuse, or a project without a `site.toml` when the certificate, the
/// wallet or the probe needs its domain.
pub async fn configure(paths: &ProjectPaths, options: &mut SiteOptions) -> Result<Vec<String>> {
    let mut set = Vec::new();
    let domain = || -> Result<String> {
        let text = std::fs::read_to_string(paths.root().join("site.toml"))
            .map_err(|e| config(format!("{}: {e}", paths.root().join("site.toml").display())))?;
        Ok(adk_site::content::SiteConfig::from_toml(&text).map_err(config)?.domain)
    };

    if let Some(email) = read_json::<EmailConfig>(&paths.root().join("email.json"))? {
        let password = secret_in(paths, "email.json", &email.password)?;
        let mut driver = ImapSmtpEmail::new(
            email.imap.endpoint()?,
            email.smtp.endpoint()?,
            &email.username,
            password.expose(),
        )
        .map_err(config)?;
        if let Some(mailbox) = &email.mailbox {
            driver = driver.with_mailbox(mailbox.clone());
        }
        let address = email.from.clone().unwrap_or_else(|| email.username.clone());
        let mut from = Mailbox::new(&address);
        if let Some(name) = &email.from_name {
            from = from.with_name(name.clone());
        }
        let domain_auth = match &email.sender_policy {
            Some(policy) => {
                let sender = address.rsplit('@').next().unwrap_or_default().to_string();
                let lookup = SystemTxtLookup::new().map_err(config)?;
                Some((
                    Arc::new(lookup) as Arc<dyn adk_drivers::email::TxtLookup>,
                    SenderPolicy::new(sender, &policy.dkim_selector, &policy.spf_mechanism),
                ))
            }
            None => None,
        };
        options.email = Some(EmailOptions { driver: Arc::new(driver), from, domain_auth });
        set.push(format!("email as {address}"));
    }

    if let Some(upkeep) = read_json::<UpkeepConfig>(&paths.root().join("upkeep.json"))? {
        match (&upkeep.lockfile, &upkeep.advisories) {
            (Some(lockfile), Some(advisories)) => {
                options.advisories = Some((
                    paths.root().join(lockfile),
                    Arc::new(crate::upkeep::AdvisoryDatabase::new(paths.root().join(advisories))),
                ));
                set.push(format!(
                    "advisories from {} against {}",
                    advisories.display(),
                    lockfile.display()
                ));
            }
            (None, None) => {}
            _ => {
                return Err(config(
                    "upkeep.json: name both the `lockfile` and the `advisories` database",
                ));
            }
        }
        if upkeep.certificate {
            let origin = url::Url::parse(&format!("https://{}", domain()?)).map_err(config)?;
            let only =
                std::collections::BTreeSet::from([adk_site::migrate::http::origin_of(&origin)]);
            let http = adk_site::migrate::http::HttpClient::builder()
                .egress(adk_site::migrate::http::EgressScope::Only(only))
                .build()
                .map_err(config)?;
            options.certificate =
                Some(Arc::new(crate::upkeep::OriginCertificate::new(Arc::new(http), origin)));
            set.push("the origin's certificate".to_string());
        }
    }

    if let Some(wallet) = read_json::<WalletConfig>(&paths.root().join("wallet.json"))? {
        wallet.policy.validate()?;
        let id = domain()?.replace('.', "-");
        let currency = wallet.policy.currency.clone();
        let mut builder =
            Wallet::builder(id, wallet.policy.clone(), Arc::new(NoStoredValue::new(&currency)));
        for provider in &wallet.billing {
            let dir = paths.data().join("wallet").join(provider);
            builder = builder.rail(Arc::new(ProviderBillingRail::new(Arc::new(
                FileBillingAccount::new(provider.clone(), dir),
            ))));
        }
        std::fs::create_dir_all(paths.data()).map_err(config)?;
        let pool = sqlx::SqlitePool::connect(&paths.database("wallet")).await.map_err(config)?;
        let store = adk_payments::wallet::SqliteWalletStore::new(pool);
        store.migrate().await?;
        let mut builder = builder.store(Arc::new(store));
        if !options.owner.is_empty() {
            builder = builder.owner(SubjectRef::human(options.owner.clone()));
        }
        options.wallet = Some(builder.build()?);
        set.push(format!("the wallet, billed through {}", wallet.billing.join(", ")));
    }

    let claim =
        domain().ok().map(|domain| paths.data().join("claims").join(format!("{domain}.json")));
    if options.probe.is_none() && claim.is_some_and(|claim| claim.exists()) {
        options.probe = Some(ProbeOptions::system());
        set.push("the Guardian's probe of the claimed domain".to_string());
    }
    Ok(set)
}
