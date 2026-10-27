//! `adk-rust site packs`: capability packs as signed bundles (spec 3.5 §7.23).
//!
//! | Command | Library |
//! |---|---|
//! | `site packs list` | `PackDir::load`: the built-in packs, the installed ones, and the files the site does not load |
//! | `site packs install FILE` | `PackDir::plan` and `install`: verified against `packs/trust.json`, refused when unsigned, tampered, from an unknown publisher, invalid, conflicting, a downgrade or a republished version |
//! | `site packs remove NAME` | `PackDir::remove` |
//! | `site packs trust PUBLISHER KEY` | `PackDir::trust_key`: a publisher's Ed25519 public key |
//! | `site packs sign FILE --key KEY` | `PackBundle::check` and `seal`: the publisher's side |
//!
//! Installing, removing and trusting are the person's acts at the terminal: each shows what
//! it does and asks, and `--yes` answers for a script.

use std::path::{Path, PathBuf};

use adk_awp::signing::jws::{DocumentSigner as _, Ed25519DocumentSigner, jwk_thumbprint};
use adk_website::deploy::project::KeyFile;
use adk_website::packs::Pack;
use adk_website::packs::bundle::{PackBundle, PackError, VerifiedPack};
use adk_website::packs::installed::{Admission, Installation, PackDir, PackOrigin};
use adk_website::packs::trust::TrustedKey;
use anyhow::{Context, Result, bail};
use awp_types::PublicKeyJwk;
use serde_json::{Value, json};

use super::confirm;

/// `adk-rust site packs` subcommands (spec 3.5 §7.23).
#[derive(clap::Subcommand, Debug, Clone)]
pub enum PacksCommands {
    /// The packs the site loads: built-in and installed, each with its version, publisher,
    /// digest and what it adds, and the installed files it does not load.
    List {
        /// Print the packs as JSON
        #[arg(long)]
        json: bool,
    },

    /// Install a signed pack bundle. It is verified against the publisher keys in
    /// packs/trust.json and refused when unsigned, tampered, from an unknown publisher,
    /// invalid, in conflict with the site's packs, or older than the installed version.
    ///
    /// Example: `adk-rust site packs install waitlist-1.0.0.pack.json`
    Install {
        /// The bundle file
        file: PathBuf,
        /// Install without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Remove an installed pack and its driver manifests.
    Remove {
        /// The pack's name
        name: String,
        /// Remove without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Trust a publisher's public key (an Ed25519 JWK) to sign the packs this site installs.
    ///
    /// Example: `adk-rust site packs trust tables.example tables-key.json`
    Trust {
        /// The publisher's id, as its bundles name it
        publisher: String,
        /// The publisher's public key: a JWK file with kty OKP, crv Ed25519 and x
        key: PathBuf,
        /// The key id its bundles' proofs name (default: the JWK's kid)
        #[arg(long)]
        kid: Option<String>,
        /// Where the key came from, kept with it
        #[arg(long)]
        note: Option<String>,
        /// Trust it without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Seal a pack as its publisher: check it, set its content digest and sign it with the
    /// publisher's key file (a private Ed25519 JWK, as `adk-rust awp keys` writes).
    ///
    /// Example: `adk-rust site packs sign waitlist.json --key publisher.jwk`
    Sign {
        /// The unsigned bundle
        file: PathBuf,
        /// The publisher's private key file
        #[arg(long, value_name = "FILE")]
        key: PathBuf,
        /// Where to write the signed bundle (default: NAME-VERSION.pack.json beside FILE)
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
}

/// Runs an `adk-rust site packs` command on the project at `root`.
pub(crate) async fn run(root: &Path, command: PacksCommands) -> Result<()> {
    let packs = PackDir::new(root);
    match command {
        PacksCommands::List { json } => list(&packs, json),
        PacksCommands::Install { file, yes } => install(&packs, &file, yes),
        PacksCommands::Remove { name, yes } => remove(&packs, &name, yes),
        PacksCommands::Trust { publisher, key, kid, note, yes } => {
            trust(&packs, &publisher, &key, kid, note, yes)
        }
        PacksCommands::Sign { file, key, out } => sign(&file, &key, out).await,
    }
}

fn origin(origin: &PackOrigin) -> &'static str {
    match origin {
        PackOrigin::BuiltIn => "built-in",
        PackOrigin::Installed { .. } => "installed",
    }
}

fn json_name(value: impl serde::Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// `adk-rust site packs list`.
fn list(packs: &PackDir, as_json: bool) -> Result<()> {
    let set = packs.load()?;
    if as_json {
        let listed: Vec<Value> = set
            .packs()
            .iter()
            .map(|pack| {
                let bundle = pack.bundle();
                json!({
                    "name": bundle.name,
                    "version": bundle.version,
                    "publisher": bundle.publisher.id,
                    "origin": origin(&pack.origin),
                    "digest": pack.pack.digest,
                    "kid": pack.pack.kid,
                    "capabilities": bundle.capabilities.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                    "playbooks": bundle.playbooks.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
                    "drivers": bundle.drivers.iter().map(|d| d.id.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();
        let refused: Vec<Value> = set
            .refused()
            .iter()
            .map(|refused| {
                json!({
                    "file": refused.file.display().to_string(),
                    "code": refused.error.code(),
                    "error": refused.error.to_string(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "packs": listed, "refused": refused }))?
        );
        return Ok(());
    }
    for pack in set.packs() {
        let bundle = pack.bundle();
        println!(
            "{} {} by {} ({}): {}",
            bundle.name,
            bundle.version,
            bundle.publisher.id,
            origin(&pack.origin),
            bundle.description
        );
        if !bundle.capabilities.is_empty() {
            let names: Vec<&str> = bundle.capabilities.iter().map(|c| c.name.as_str()).collect();
            println!("  capabilities: {}", names.join(", "));
        }
        if !bundle.playbooks.is_empty() {
            let ids: Vec<&str> = bundle.playbooks.iter().map(|p| p.id.as_str()).collect();
            println!("  playbooks: {}", ids.join(", "));
        }
        for driver in &bundle.drivers {
            println!(
                "  driver {}: {}",
                driver.id,
                packs.drivers_dir(&bundle.name).join(format!("{}.toml", driver.id)).display()
            );
        }
    }
    for refused in set.refused() {
        println!("Not loaded: {}: {}", refused.file.display(), refused.error);
    }
    Ok(())
}

/// What a verified pack adds, as the person reads it before deciding.
fn describe(pack: &VerifiedPack, drivers: &[PathBuf]) {
    let bundle = &pack.bundle;
    let publisher = match &bundle.publisher.name {
        Some(name) => format!("{} ({name})", bundle.publisher.id),
        None => bundle.publisher.id.clone(),
    };
    println!("Pack {} {} by {publisher}: {}", bundle.name, bundle.version, bundle.description);
    println!(
        "  Signed with key {} (thumbprint {}); digest {}",
        pack.kid, pack.thumbprint, pack.digest
    );
    for capability in &bundle.capabilities {
        let handler = bundle.handlers.get(&capability.name).map(|h| h.label()).unwrap_or_default();
        println!(
            "  capability {}: {}, for {} and above, served by {handler}",
            capability.name,
            json_name(capability.effect),
            json_name(capability.min_trust)
        );
    }
    for playbook in &bundle.playbooks {
        let agents: Vec<&str> = playbook.agents.iter().map(|agent| agent.as_str()).collect();
        println!("  playbook {}: the {} job, for {}", playbook.id, playbook.job, agents.join(", "));
    }
    for (driver, file) in bundle.drivers.iter().zip(drivers) {
        println!(
            "  driver {}: mount it with `adk-rust drivers mount {}` once installed",
            driver.id,
            file.display()
        );
    }
}

/// `adk-rust site packs install`.
fn install(packs: &PackDir, file: &Path, yes: bool) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    let plan = packs.plan(&bytes)?;
    let Installation { pack, admission, drivers } = &plan;
    let name = &pack.bundle.name;
    describe(pack, drivers);
    match admission {
        Admission::Unchanged => {
            println!("{name} {} is installed already; nothing changed.", pack.bundle.version);
            return Ok(());
        }
        Admission::New => println!("Installs {name} {}.", pack.bundle.version),
        Admission::Upgrade { from } => {
            println!("Upgrades {name} from {from} to {}.", pack.bundle.version);
        }
    }
    if !confirm("Install it?", yes)? {
        println!("Not installed.");
        return Ok(());
    }
    packs.install(&bytes)?;
    println!(
        "Installed {name} {} in {}. The site loads it when it next starts.",
        pack.bundle.version,
        packs.bundle_file(name).display()
    );
    Ok(())
}

/// `adk-rust site packs remove`.
fn remove(packs: &PackDir, name: &str, yes: bool) -> Result<()> {
    let set = packs.load()?;
    let drivers: Vec<String> = set
        .get(name)
        .filter(|pack| matches!(pack.origin, PackOrigin::Installed { .. }))
        .map(|pack| pack.bundle().drivers.iter().map(|d| d.id.clone()).collect())
        .unwrap_or_default();
    if !packs.bundle_file(name).is_file() {
        let builtin = Pack::ALL.iter().any(|pack| pack.as_str() == name);
        return Err(if builtin {
            PackError::BuiltIn(name.to_string())
        } else {
            PackError::NotInstalled(name.to_string())
        }
        .into());
    }
    if !confirm(&format!("Remove pack {name}?"), yes)? {
        println!("Not removed.");
        return Ok(());
    }
    packs.remove(name)?;
    println!("Removed {name}. The site drops it when it next starts.");
    for driver in drivers {
        println!("If you mounted its driver, unmount it: `adk-rust drivers unmount {driver}`");
    }
    Ok(())
}

/// `adk-rust site packs trust`.
fn trust(
    packs: &PackDir,
    publisher: &str,
    key_file: &Path,
    kid: Option<String>,
    note: Option<String>,
    yes: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(key_file)
        .with_context(|| format!("cannot read {}", key_file.display()))?;
    let jwk: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a JSON Web Key", key_file.display()))?;
    if jwk.get("d").is_some() {
        bail!(
            "{} holds a private key; trust the publisher's public key (kty, crv, x), and keep \
             private keys off this machine",
            key_file.display()
        );
    }
    let public_key: PublicKeyJwk = serde_json::from_value(jwk.clone())
        .with_context(|| format!("{} is not an Ed25519 public JWK", key_file.display()))?;
    let Some(kid) = kid.or_else(|| jwk.get("kid").and_then(Value::as_str).map(str::to_string))
    else {
        bail!("the key names no kid; pass the id its bundles' proofs name with --kid");
    };
    let mut key = TrustedKey::new(publisher, kid.clone(), public_key);
    if let Some(note) = note {
        key = key.noted(note);
    }
    println!(
        "Trust key {kid} of publisher {publisher}, thumbprint {}? Compare the thumbprint with the \
         one the publisher gave you: every pack signed with this key will install.",
        key.thumbprint()
    );
    if !confirm("Trust this key?", yes)? {
        println!("Not trusted.");
        return Ok(());
    }
    if packs.trust_key(key)? {
        println!("Trusted. {} lists it.", packs.trust_file().display());
    } else {
        println!("{} trusts that key already.", packs.trust_file().display());
    }
    Ok(())
}

/// `adk-rust site packs sign`.
async fn sign(file: &Path, key_file: &Path, out: Option<PathBuf>) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    let bundle = PackBundle::parse(&bytes)?;
    bundle.check()?;
    let key = KeyFile::read(key_file)?;
    let signer = Ed25519DocumentSigner::from_seed(key.kid.clone(), key.seed())?;
    let sealed = bundle.seal(&signer).await?;
    let out = out.unwrap_or_else(|| {
        file.with_file_name(format!("{}-{}.pack.json", sealed.name, sealed.version))
    });
    std::fs::write(&out, sealed.to_json()?)
        .with_context(|| format!("cannot write {}", out.display()))?;
    let public = signer.public_key();
    println!(
        "Signed {} {} as {}; digest {}.",
        sealed.name,
        sealed.version,
        out.display(),
        sealed.digest.map(|digest| digest.to_string()).unwrap_or_default()
    );
    println!(
        "Owners trust this key with `adk-rust site packs trust {} KEY_FILE`; its thumbprint is {}:",
        sealed.publisher.id,
        jwk_thumbprint(&public)
    );
    println!("{}", json!({ "kty": public.kty, "crv": public.crv, "kid": key.kid, "x": public.x }));
    Ok(())
}
