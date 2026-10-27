//! `adk-rust awp keys rotate|delegate`: the site's root and delegated keys (spec 3.5 §7.2,
//! §8.1, D-11, A-I5).
//!
//! Both are root operations: they read the root key file, so they run only at a terminal and
//! ask the person to confirm. An agent's shell has no terminal, so no agent runs them. Each
//! writes the key set to `<site>/well-known/awp/keys.json` and signs the contract again with
//! the root into `<site>/well-known/awp.json`; `site build` and `site publish` release both.
//! Each then appends `awp.keys.rotated` or `awp.keys.delegated` to the site's audit trail,
//! `<site>/data/audit.jsonl`, which a served site writes too (spec 3.5 §9.3).

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use adk_awp::signing::anchor::{root_record, root_record_name};
use adk_awp::signing::bundle::StaticBundle;
use adk_awp::signing::jws::DocumentSigner;
use adk_awp::signing::keys::{delegate, endorse, revoke, verify_key_set};
use anyhow::{Context, Result, bail};
use awp_types::v2::contract::{CHECKPOINT_PATH, release_manifest_path};
use awp_types::{
    ContactLinks, ContractRelease, KeyScope, KeySet, Profile, PublicTerms, RetiredRoot, RootKey,
    SiteContract, SiteRef, SurfaceLinks,
};
use chrono::{Duration, Utc};

use super::keyfile::KeyFile;
use crate::site_cli::project::SitePaths;

/// `adk-rust awp keys` subcommands (spec 3.5 §8.1).
#[derive(clap::Subcommand, Debug, Clone)]
pub enum KeysCommands {
    /// Make a new root key, endorsed by the current one, and sign the key set and contract
    /// with it; the site's first root when it has none.
    ///
    /// Prints the `_awp-root` DNS record to publish. Example: `adk-rust awp keys rotate
    /// --root-key ~/keys/root-2026.jwk --new-root-key ~/keys/root-2027.jwk`
    Rotate(RotateArgs),

    /// Delegate a key — the release key the site signs its log with, or a card, receipt or
    /// payments key — for a validity window, signed by the root.
    ///
    /// Example: `adk-rust awp keys delegate --root-key ~/keys/root.jwk --scope release`
    Delegate(DelegateArgs),
}

/// `adk-rust awp keys rotate`.
#[derive(clap::Args, Debug, Clone)]
pub struct RotateArgs {
    /// The site project (default: the current directory)
    #[arg(long, default_value = ".")]
    pub site: PathBuf,
    /// The current root key file; omit it for the site's first root
    #[arg(long, value_name = "FILE")]
    pub root_key: Option<PathBuf>,
    /// Where to write the new root key file, which must not exist; keep it off the site's
    /// host and out of every agent's reach
    #[arg(long, value_name = "FILE")]
    pub new_root_key: PathBuf,
    /// The new root's key id (default: site-root-{year}-{month})
    #[arg(long)]
    pub kid: Option<String>,
    /// The current root key is lost or compromised: the new root is anchored by the
    /// `_awp-root` DNS record alone, and every delegation is made again
    #[arg(long, conflicts_with = "root_key")]
    pub recover: bool,
}

/// What a delegated key may sign.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeArg {
    /// Site log checkpoints, and through them releases
    Release,
    /// A2A Agent Cards
    Card,
    /// Receipts, consent receipts, status and stream entries the edge issues
    Receipt,
    /// Quotes and payment mandates
    Payments,
}

impl ScopeArg {
    fn scope(self) -> KeyScope {
        match self {
            Self::Release => KeyScope::Release,
            Self::Card => KeyScope::Card,
            Self::Receipt => KeyScope::Receipt,
            Self::Payments => KeyScope::Payments,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Card => "card",
            Self::Receipt => "receipt",
            Self::Payments => "payments",
        }
    }
}

/// `adk-rust awp keys delegate`.
#[derive(clap::Args, Debug, Clone)]
pub struct DelegateArgs {
    /// The site project (default: the current directory)
    #[arg(long, default_value = ".")]
    pub site: PathBuf,
    /// The root key file
    #[arg(long, value_name = "FILE")]
    pub root_key: PathBuf,
    /// What the key may sign; repeat or separate with commas
    #[arg(long, value_enum, value_delimiter = ',', required = true)]
    pub scope: Vec<ScopeArg>,
    /// An existing private key file to delegate again, such as data/keys/release.jwk
    #[arg(long, value_name = "FILE", conflicts_with = "out")]
    pub key: Option<PathBuf>,
    /// Where to write a new key (default: {site}/data/keys/{scope}.jwk)
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// The new key's id (default: {scope}-{year}-{month})
    #[arg(long)]
    pub kid: Option<String>,
    /// Days the delegation stays valid
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u32).range(1..=366))]
    pub days: u32,
}

/// Runs an `awp keys` command.
pub(crate) async fn run(command: KeysCommands) -> Result<()> {
    match command {
        KeysCommands::Rotate(args) => rotate(args).await,
        KeysCommands::Delegate(args) => delegate_key(args).await,
    }
}

/// Root operations run only for a person at a terminal (spec 3.5 §8.1, A-I5).
fn at_a_terminal(operation: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "`awp keys {operation}` uses the root key, so it runs only for a person at a \
             terminal, never inside an agent or a pipeline (spec 3.5 A-I5); run it in your own \
             terminal"
        );
    }
    Ok(())
}

fn confirm(question: &str) -> Result<()> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        bail!("nothing changed");
    }
    Ok(())
}

/// A root key was made or rotated (spec 3.5 §9.3).
pub(crate) const KEYS_ROTATED: &str = "awp.keys.rotated";
/// A key was delegated by the root.
pub(crate) const KEYS_DELEGATED: &str = "awp.keys.delegated";

/// Appends `action`, done under the root key `root`, to the site's audit trail. The keys are
/// already written, so a record that cannot be written is reported, not undone.
async fn record(
    paths: &SitePaths,
    domain: &str,
    action: &str,
    root: &str,
    fields: serde_json::Value,
) {
    use adk_core::audit_log::{
        AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecorder, FileAuditRecorder,
    };
    let path = paths.data().join("audit.jsonl");
    let written = async {
        let recorder = FileAuditRecorder::open(&path)?.with_log_id(domain);
        let mut draft = AuditRecordDraft::new(AuditRecordKind::Other("awp".into()), action)
            .actor(AuditActor::subject(adk_core::principal::SubjectRef::human(format!(
                "awp-root:{root}"
            ))))
            .args_digest(recorder.digest(&fields))
            .ext("site", serde_json::json!(domain))
            .ext("principal", serde_json::json!(format!("awp-root:{root}")));
        if let serde_json::Value::Object(fields) = fields {
            for (key, value) in fields {
                draft = draft.ext(key, value);
            }
        }
        recorder.record(draft).await
    };
    if let Err(error) = written.await {
        eprintln!("warning: {action} was not added to {}: {error}", path.display());
    }
}

fn month_kid(prefix: &str) -> String {
    format!("{prefix}-{}", Utc::now().format("%Y-%m"))
}

fn read_keys(path: &Path) -> Result<Option<KeySet>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not an AWP key set", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn read_root(path: &Path, keys: &KeySet) -> Result<KeyFile> {
    let root = KeyFile::read(path)?;
    if root.kid() != keys.root.kid || !root.is(&keys.root.public_key) {
        bail!(
            "{} is not the site's root key `{}`; give the root key file the key set names",
            path.display(),
            keys.root.kid
        );
    }
    Ok(root)
}

async fn rotate(args: RotateArgs) -> Result<()> {
    at_a_terminal("rotate")?;
    let paths = SitePaths::new(&args.site);
    let config = paths.config()?;
    let domain = config.domain.clone();
    let current = read_keys(&paths.keys_json())?;
    let old = match (&current, &args.root_key, args.recover) {
        (None, Some(_), _) => bail!(
            "{} holds no key set yet; omit --root-key to make the site's first root",
            paths.keys_json().display()
        ),
        (Some(keys), Some(path), _) => Some(read_root(path, keys)?),
        (Some(keys), None, false) => bail!(
            "the site's root is `{}`: pass --root-key FILE to rotate it, or --recover when the \
             root key is lost",
            keys.root.kid
        ),
        (_, None, _) => None,
    };
    let new = KeyFile::generate(&args.kid.clone().unwrap_or_else(|| month_kid("site-root")))?;
    if current.as_ref().is_some_and(|keys| {
        keys.root.kid == new.kid() || keys.retired_roots.iter().any(|r| r.kid == new.kid())
    }) {
        bail!("key id `{}` was a root of this site already; choose another with --kid", new.kid());
    }
    let action = match (&current, &old) {
        (None, _) => format!("Make `{}` the first root key of {domain}", new.kid()),
        (Some(keys), Some(_)) => format!(
            "Rotate the root of {domain} from `{}` to `{}`, endorsed by the current root",
            keys.root.kid,
            new.kid()
        ),
        (Some(keys), None) => format!(
            "Recover {domain} with root `{}`: `{}` is dropped with every delegation it signed",
            new.kid(),
            keys.root.kid
        ),
    };
    confirm(&format!("{action}?"))?;

    let new_signer = new.document_signer()?;
    let now = Utc::now();
    let keys = match (current, &old) {
        (None, _) => KeySet {
            root: RootKey {
                kid: new.kid().into(),
                public_key: new.public_key(),
                endorsement: None,
            },
            delegations: Vec::new(),
            retired_roots: Vec::new(),
            revocations: Vec::new(),
        },
        (Some(keys), Some(old)) => {
            let old_signer = old.document_signer()?;
            let mut retired = keys.retired_roots.clone();
            retired.push(RetiredRoot {
                kid: keys.root.kid.clone(),
                public_key: keys.root.public_key.clone(),
                retired_at: now,
            });
            KeySet {
                root: endorse(&old_signer, &new_signer).await?,
                delegations: keys.delegations.clone(),
                retired_roots: retired,
                revocations: resign_revocations(&keys, &new_signer).await?,
            }
        }
        (Some(keys), None) => KeySet {
            root: RootKey {
                kid: new.kid().into(),
                public_key: new.public_key(),
                endorsement: None,
            },
            delegations: Vec::new(),
            retired_roots: Vec::new(),
            revocations: resign_revocations(&keys, &new_signer).await?,
        },
    };
    verify_key_set(&keys).map_err(|error| anyhow::anyhow!("the new key set: {error}"))?;
    // The new root's file first: if it cannot be written, nothing else changes.
    new.write_new(&args.new_root_key)?;
    write_documents(&paths, &config, &keys, &new_signer).await?;
    let previous = keys.retired_roots.last().map(|root| root.kid.clone());
    record(
        &paths,
        &domain,
        KEYS_ROTATED,
        old.as_ref().map_or(new.kid(), |old| old.kid()),
        serde_json::json!({
            "root": new.kid(),
            "previousRoot": old.as_ref().and(previous),
            "first": old.is_none() && !args.recover,
            "recovered": args.recover,
            "delegations": keys.delegations.len(),
        }),
    )
    .await;
    println!("New root `{}` written to {}.", new.kid(), args.new_root_key.display());
    println!("Signed {} and the contract with it.", paths.keys_json().display());
    println!(
        "Publish this DNS record (replace any earlier one) — a person changes it at the DNS provider:"
    );
    println!(
        "  {} TXT \"{}\"",
        root_record_name(&domain),
        root_record(new.kid(), &new.public_key())
    );
    match &old {
        Some(_) => println!(
            "Verifiers that pinned the old root accept the new one through its endorsement."
        ),
        None if args.recover => println!(
            "Delegate the release key again (`adk-rust awp keys delegate --scope release`); \
             verifiers that pinned the old root will report a recovery."
        ),
        None => {
            let release = paths.key_file("release");
            let key = match release.exists() {
                true => format!(" --key {}", release.display()),
                false => String::new(),
            };
            println!(
                "Next: delegate the release key with `adk-rust awp keys delegate --root-key {} \
                 --scope release{key}`.",
                args.new_root_key.display()
            );
        }
    }
    println!("Keep the root key file off the site's host and out of every agent's reach.");
    Ok(())
}

/// Revocations stay in force under a new root: only the current root revokes.
async fn resign_revocations(
    keys: &KeySet,
    root: &dyn DocumentSigner,
) -> Result<Vec<awp_types::Revocation>> {
    let mut revocations = Vec::new();
    for revocation in &keys.revocations {
        revocations.push(revoke(root, revocation.kid.clone(), revocation.revoked_at).await?);
    }
    Ok(revocations)
}

async fn delegate_key(args: DelegateArgs) -> Result<()> {
    at_a_terminal("delegate")?;
    let paths = SitePaths::new(&args.site);
    let config = paths.config()?;
    let mut keys = read_keys(&paths.keys_json())?.with_context(|| {
        format!(
            "{} holds no key set; make the site's root first with `adk-rust awp keys rotate`",
            paths.keys_json().display()
        )
    })?;
    let root = read_root(&args.root_key, &keys)?;
    let first = args.scope[0];
    let (key, written) = match &args.key {
        Some(path) => (KeyFile::read(path)?, None),
        None => {
            let out = args.out.clone().unwrap_or_else(|| paths.key_file(first.name()));
            if out.exists() {
                bail!(
                    "{} exists; delegate it again with --key {}, or write a new key elsewhere \
                     with --out",
                    out.display(),
                    out.display()
                );
            }
            let kid = args.kid.clone().unwrap_or_else(|| month_kid(first.name()));
            (KeyFile::generate(&kid)?, Some(out))
        }
    };
    if key.kid() == keys.root.kid || key.is(&keys.root.public_key) {
        bail!("the root key is never delegated; delegate a key of its own");
    }
    let mut scopes: Vec<KeyScope> = args.scope.iter().map(|scope| scope.scope()).collect();
    scopes.sort();
    scopes.dedup();
    let names: Vec<&str> = args.scope.iter().map(|scope| scope.name()).collect();
    let not_before = Utc::now();
    let not_after = not_before + Duration::days(i64::from(args.days));
    confirm(&format!(
        "Delegate `{}` for {} on {} until {}?",
        key.kid(),
        names.join(", "),
        config.domain,
        not_after.format("%Y-%m-%d")
    ))?;
    let root_signer = root.document_signer()?;
    let delegation =
        delegate(&root_signer, key.kid(), key.public_key(), scopes, not_before, not_after).await?;
    keys.delegations.retain(|existing| existing.kid != key.kid());
    keys.delegations.push(delegation);
    verify_key_set(&keys).map_err(|error| anyhow::anyhow!("the new key set: {error}"))?;
    if let Some(out) = &written {
        key.write_new(out)?;
    }
    write_documents(&paths, &config, &keys, &root_signer).await?;
    record(
        &paths,
        &config.domain,
        KEYS_DELEGATED,
        root.kid(),
        serde_json::json!({
            "kid": key.kid(),
            "scopes": names,
            "notBefore": not_before,
            "notAfter": not_after,
            "newKey": written.is_some(),
        }),
    )
    .await;
    match &written {
        Some(out) => println!("New key `{}` written to {}.", key.kid(), out.display()),
        None => println!("Delegated `{}`.", key.kid()),
    }
    println!(
        "Delegated for {} until {}; signed {} and the contract again.",
        names.join(", "),
        not_after.format("%Y-%m-%d %H:%M UTC"),
        paths.keys_json().display()
    );
    println!("The next `adk-rust site publish` releases them.");
    Ok(())
}

/// Writes the key set and the root-signed contract into `<site>/well-known/` (spec 3.5
/// §7.2): the contract the site already has, or one for the static profile from `site.toml`.
async fn write_documents(
    paths: &SitePaths,
    config: &adk_site::content::SiteConfig,
    keys: &KeySet,
    root: &dyn DocumentSigner,
) -> Result<()> {
    let origin = format!("https://{}", config.domain);
    let latest = paths.latest_release().await?.unwrap_or(1).max(1);
    let mut contract = match std::fs::read(paths.contract()) {
        Ok(bytes) => serde_json::from_slice::<SiteContract>(&bytes)
            .with_context(|| format!("{} is not an AWP contract", paths.contract().display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut contract = SiteContract::new(SiteRef {
                id: paths.site_id(),
                name: config.name.clone(),
                domain: config.domain.clone(),
                languages: config.languages.clone(),
            });
            contract.profile = Some(Profile::Static);
            contract.surfaces = Some(SurfaceLinks {
                a2a_card: None,
                mcp_server_card: None,
                manifest: None,
                llms_txt: Some(format!("{origin}/llms.txt")),
                machine_views: true,
            });
            if !config.security.contact.is_empty() {
                contract.contact = Some(ContactLinks {
                    security: Some(format!("{origin}/.well-known/security.txt")),
                    email: None,
                    page: None,
                });
            }
            contract
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot read {}", paths.contract().display()));
        }
    };
    if contract.site.domain != config.domain {
        bail!(
            "{} names {} but site.toml names {}; one site, one domain",
            paths.contract().display(),
            contract.site.domain,
            config.domain
        );
    }
    contract.release = Some(ContractRelease {
        seq: latest,
        manifest: format!("{origin}{}", release_manifest_path(latest)),
        checkpoint: format!("{origin}{CHECKPOINT_PATH}"),
    });
    // The generator publishes the charter's public terms from public-terms.json, and the
    // contract carries their digest.
    let terms = match paths.public_terms()? {
        Some(terms) => terms,
        None => {
            let terms = PublicTerms::default();
            let path = paths.root().join("public-terms.json");
            std::fs::write(&path, super::pretty(&terms)?)
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("Wrote empty public terms to {}; the charter fills them.", path.display());
            terms
        }
    };
    let bundle = StaticBundle::build(contract, keys, &terms, root).await?;
    for (path, bytes) in bundle.files() {
        if path == awp_types::v2::contract::CHARTER_PATH {
            continue;
        }
        let target = paths.well_known_file(path)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::write(&target, bytes)
            .with_context(|| format!("cannot write {}", target.display()))?;
    }
    Ok(())
}
