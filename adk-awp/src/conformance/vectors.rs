//! The AWP 2.0 test vectors: signed contract, signed delegation, signed Agent Card and release
//! checkpoint (spec 3.5 §7.7, §11 "Interoperability"; the protocol repository's
//! `vectors/README.md`).
//!
//! Each case is one JSON file under `<vector>/<case>.json` with its inputs, every intermediate
//! value (bytes as base64url) and the expected outcome: `{"valid": true}` or
//! `{"valid": false, "failure": "<AWP 2.0 §15.2 name>"}`. Generation uses fixed seeds and times,
//! so the files are byte-identical on every run; [`check_case`] evaluates a case with this
//! crate's verifier code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_auth::signed_audit::merkle::{
    consistency_proof, inclusion_proof, leaf_hash, root, verify_consistency, verify_inclusion,
};
use adk_auth::signed_audit::{CheckpointBody, Note};
use adk_core::audit_log::AuditHash;
use awp_types::{Delegation, KeyScope, KeySet, LogEntry, PublicKeyJwk, Revocation, SiteContract};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use super::fixture::{EDGE_SEED, FixtureSite, RELEASE_SEED, ROOT_SEED, fixture_time};
use crate::signing::SigningError;
use crate::signing::anchor::{RootRecord, root_record, root_record_name};
use crate::signing::card::verify_agent_card;
use crate::signing::jws::{
    DocumentSigner, Ed25519DocumentSigner, jwk_thumbprint, public_key_bytes, sign, signing_payload,
    verify,
};
use crate::signing::keys::{Dated, delegated_key, revoke, verify_key_set};
use crate::signing::log::{checkpoint_verifier, entry_bytes, log_origin, signed_checkpoint};

/// The fixture domain the vectors use.
pub const VECTOR_DOMAIN: &str = "example.com";

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut out = Map::new();
            for (key, child) in entries {
                out.insert(key, sorted(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// The bytes a case is written as: sorted keys, pretty-printed, a final newline.
pub fn case_bytes(case: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&sorted(case.clone())).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

fn jws_parts(proof_jws: &str) -> (String, String) {
    let mut parts = proof_jws.split('.');
    let header = parts.next().unwrap_or_default().to_owned();
    let signature = parts.nth(1).unwrap_or_default().to_owned();
    (header, signature)
}

fn signed_values<T: serde::Serialize>(document: &T, jws: &str) -> Result<Value, SigningError> {
    let payload = signing_payload(document)?;
    let (header, signature) = jws_parts(jws);
    let header_json = URL_SAFE_NO_PAD
        .decode(&header)
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    Ok(json!({
        "jcs": b64(payload.as_bytes()),
        "protectedHeader": header_json,
        "signingInput": format!("{header}.{}", b64(payload.as_bytes())),
        "signature": signature,
    }))
}

fn valid() -> Value {
    json!({ "valid": true })
}

fn invalid(failure: &str) -> Value {
    json!({ "valid": false, "failure": failure })
}

/// Generates every case, by `<vector>/<case>.json`.
///
/// # Errors
///
/// [`SigningError`] when a document cannot be signed.
pub async fn generate() -> Result<BTreeMap<String, Value>, SigningError> {
    let site = FixtureSite::build(VECTOR_DOMAIN, awp_types::Profile::Static).await?;
    let mut cases = BTreeMap::new();
    contract_cases(&site, &mut cases).await?;
    delegation_cases(&site, &mut cases).await?;
    card_cases(&site, &mut cases)?;
    checkpoint_cases(&site, &mut cases).await?;
    Ok(cases)
}

/// Writes every case under `dir` and returns the paths written.
///
/// # Errors
///
/// [`SigningError::Key`] wrapping an I/O error, or a signing failure.
pub async fn write(dir: &Path) -> Result<Vec<PathBuf>, SigningError> {
    let mut written = Vec::new();
    for (name, case) in generate().await? {
        let path = dir.join(&name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| SigningError::Key(error.to_string()))?;
        }
        std::fs::write(&path, case_bytes(&case))
            .map_err(|error| SigningError::Key(error.to_string()))?;
        written.push(path);
    }
    Ok(written)
}

async fn contract_cases(
    site: &FixtureSite,
    cases: &mut BTreeMap<String, Value>,
) -> Result<(), SigningError> {
    let contract = site.contract.clone();
    let proof = contract.proof.clone().unwrap_or_else(|| awp_types::Proof::eddsa("", ""));
    let base = json!({
        "description": "The site contract signed by the root key: a detached JWS (EdDSA) over the JCS form of the contract without `proof` (AWP 2.0 §5.4).",
        "domain": VECTOR_DOMAIN,
        "rootSeed": b64(&ROOT_SEED),
        "rootKid": site.root.kid(),
        "rootPublicKey": site.root.public_key(),
        "rootThumbprint": jwk_thumbprint(&site.root.public_key()),
        "dnsRecord": { "name": root_record_name(VECTOR_DOMAIN), "value": root_record(site.root.kid(), &site.root.public_key()) },
    });
    let mut case = base.clone();
    merge(&mut case, signed_values(&contract, &proof.jws)?);
    merge(&mut case, json!({ "document": contract, "expected": valid() }));
    cases.insert("signed-contract/valid.json".to_owned(), case.clone());

    let mut changed = contract.clone();
    changed.site.name = "Example Solar Ltd".to_owned();
    let mut case_changed = case.clone();
    merge(
        &mut case_changed,
        json!({
            "description": "A member changed after signing: the signature no longer matches.",
            "document": changed,
            "expected": invalid("contract.signature"),
        }),
    );
    cases.insert("signed-contract/changed-member.json".to_owned(), case_changed);

    let foreign = Ed25519DocumentSigner::from_seed(site.root.kid(), [0x99; 32])?;
    let mut forged = contract.clone();
    forged.proof = None;
    forged.proof = Some(sign(&forged, &foreign).await?);
    let mut case_foreign = base.clone();
    merge(
        &mut case_foreign,
        signed_values(&forged, &forged.proof.as_ref().map(|p| p.jws.clone()).unwrap_or_default())?,
    );
    merge(
        &mut case_foreign,
        json!({
            "description": "A contract signed by a foreign key that claims the root's kid.",
            "document": forged,
            "expected": invalid("contract.signature"),
        }),
    );
    cases.insert("signed-contract/foreign-root.json".to_owned(), case_foreign);

    let crit_header =
        json!({ "alg": "EdDSA", "kid": site.root.kid(), "crit": ["exp"] }).to_string();
    let payload = signing_payload(&contract)?;
    let input = format!("{}.{}", b64(crit_header.as_bytes()), b64(payload.as_bytes()));
    let signature = site.root.sign(input.as_bytes()).await?;
    let mut with_crit = contract.clone();
    with_crit.proof = Some(awp_types::Proof::eddsa(
        site.root.kid(),
        format!("{}..{}", b64(crit_header.as_bytes()), b64(&signature)),
    ));
    let mut case_crit = case.clone();
    merge(
        &mut case_crit,
        json!({
            "description": "A protected header with `crit`, which AWP proofs never carry, signed by the root.",
            "document": with_crit,
            "protectedHeader": crit_header,
            "expected": invalid("contract.signature"),
        }),
    );
    cases.insert("signed-contract/crit-header.json".to_owned(), case_crit);

    let (header, sig) = jws_parts(&proof.jws);
    let mut attached = contract.clone();
    attached.proof = Some(awp_types::Proof::eddsa(
        site.root.kid(),
        format!("{header}.{}.{sig}", b64(payload.as_bytes())),
    ));
    let mut case_attached = case.clone();
    merge(
        &mut case_attached,
        json!({
            "description": "A JWS whose payload part is not empty: AWP proofs are detached.",
            "document": attached,
            "expected": invalid("contract.signature"),
        }),
    );
    cases.insert("signed-contract/payload-not-detached.json".to_owned(), case_attached);

    let mut case_unanchored = case_foreign_keys(site, &foreign, &forged)?;
    merge(
        &mut case_unanchored,
        json!({
            "description": "A host attacker replaces keys.json with its own root and re-signs the contract; the DNS record still names the owner's root.",
            "expected": invalid("root.unanchored"),
        }),
    );
    cases.insert("signed-contract/unanchored-root.json".to_owned(), case_unanchored);
    Ok(())
}

fn case_foreign_keys(
    site: &FixtureSite,
    foreign: &Ed25519DocumentSigner,
    forged: &SiteContract,
) -> Result<Value, SigningError> {
    let mut case = json!({
        "domain": VECTOR_DOMAIN,
        "rootKid": foreign.kid(),
        "rootPublicKey": foreign.public_key(),
        "rootThumbprint": jwk_thumbprint(&foreign.public_key()),
        "dnsRecord": { "name": root_record_name(VECTOR_DOMAIN), "value": root_record(site.root.kid(), &site.root.public_key()) },
        "document": forged,
    });
    merge(
        &mut case,
        signed_values(forged, &forged.proof.as_ref().map(|p| p.jws.clone()).unwrap_or_default())?,
    );
    Ok(case)
}

fn merge(target: &mut Value, extra: Value) {
    if let (Some(target), Value::Object(extra)) = (target.as_object_mut(), extra) {
        for (key, value) in extra {
            target.insert(key, value);
        }
    }
}

async fn delegation_cases(
    site: &FixtureSite,
    cases: &mut BTreeMap<String, Value>,
) -> Result<(), SigningError> {
    let delegation = site.keys.delegations[0].clone();
    let base = json!({
        "description": "A root-signed delegation of the `release` key (AWP 2.0 §5.3).",
        "rootKid": site.root.kid(),
        "rootPublicKey": site.root.public_key(),
        "delegatedSeed": b64(&RELEASE_SEED),
        "scope": "release",
        "at": fixture_time(),
    });
    let mut case = base.clone();
    merge(&mut case, signed_values(&delegation, &delegation.proof.jws)?);
    merge(&mut case, json!({ "document": delegation, "revocations": [], "expected": valid() }));
    cases.insert("signed-delegation/valid.json".to_owned(), case.clone());

    let mut widened = delegation.clone();
    widened.scopes.push(KeyScope::Card);
    let mut case_scope = case.clone();
    merge(
        &mut case_scope,
        json!({
            "description": "A scope added after signing.",
            "document": widened,
            "expected": invalid("delegation.signature"),
        }),
    );
    cases.insert("signed-delegation/changed-scope.json".to_owned(), case_scope);

    let mut case_window = case.clone();
    merge(
        &mut case_window,
        json!({
            "description": "A document dated after the delegation's notAfter.",
            "at": "2027-01-15T00:00:00Z",
            "expected": invalid("key.window"),
        }),
    );
    cases.insert("signed-delegation/outside-window.json".to_owned(), case_window);

    let revoked_at: DateTime<Utc> = fixture_time() - chrono::Duration::days(1);
    let revocation = revoke(site.root.as_ref(), &delegation.kid, revoked_at).await?;
    let mut case_revoked = case.clone();
    merge(
        &mut case_revoked,
        json!({
            "description": "A root-signed revocation of the key before the document's time.",
            "revocations": [revocation],
            "expected": invalid("key.revoked"),
        }),
    );
    cases.insert("signed-delegation/revoked.json".to_owned(), case_revoked);
    Ok(())
}

fn card_cases(site: &FixtureSite, cases: &mut BTreeMap<String, Value>) -> Result<(), SigningError> {
    let card = site.json("/.well-known/agent-card.json").unwrap_or_default();
    let delegations: Vec<Delegation> = site.keys.delegations.clone();
    let base = json!({
        "description": "An A2A Agent Card signed by a delegated `card` key: a JWS in `signatures` over the JCS form of the card without `signatures` (AWP 2.0 §7.4.2).",
        "rootKid": site.root.kid(),
        "rootPublicKey": site.root.public_key(),
        "delegations": delegations,
        "cardSeed": b64(&EDGE_SEED),
        "at": fixture_time(),
        "document": card,
        "expected": valid(),
    });
    cases.insert("signed-agent-card/valid.json".to_owned(), base.clone());

    let mut altered = card.clone();
    if let Some(skill) = altered.pointer_mut("/skills/0/description") {
        *skill = json!("Search the site and place orders");
    }
    let mut case_altered = base.clone();
    merge(
        &mut case_altered,
        json!({ "description": "A skill altered after signing.", "document": altered, "expected": invalid("card.signature") }),
    );
    cases.insert("signed-agent-card/altered-skill.json".to_owned(), case_altered);

    let mut no_card = delegations.clone();
    if let Some(edge) = no_card.iter_mut().find(|d| d.kid == site.edge_key.kid()) {
        edge.scopes.retain(|scope| *scope != KeyScope::Card);
    }
    let mut case_scope = base.clone();
    merge(
        &mut case_scope,
        json!({
            "description": "The signing key's delegation lacks the `card` scope (its root proof no longer matches either).",
            "delegations": no_card,
            "expected": invalid("card.signature"),
        }),
    );
    cases.insert("signed-agent-card/delegation-without-card.json".to_owned(), case_scope);

    let mut case_expired = base;
    merge(
        &mut case_expired,
        json!({
            "description": "Checked after the card key's delegation expired.",
            "at": "2027-01-15T00:00:00Z",
            "expected": invalid("card.signature"),
        }),
    );
    cases.insert("signed-agent-card/expired-delegation.json".to_owned(), case_expired);
    Ok(())
}

async fn checkpoint_cases(
    site: &FixtureSite,
    cases: &mut BTreeMap<String, Value>,
) -> Result<(), SigningError> {
    let entries = &site.entries;
    let leaves: Vec<AuditHash> =
        entries.iter().map(|e| entry_bytes(e).map(|b| leaf_hash(&b))).collect::<Result<_, _>>()?;
    let tree_root = root(&leaves);
    let checkpoint =
        String::from_utf8_lossy(&site.files[awp_types::v2::contract::CHECKPOINT_PATH].body)
            .to_string();
    let tiles: BTreeMap<String, String> = site
        .files
        .iter()
        .filter(|(path, _)| path.starts_with("/.well-known/awp/log/tile/"))
        .map(|(path, served)| (path.clone(), b64(&served.body)))
        .collect();
    let old_entries = &entries[..1];
    let old_root = root(&leaves[..1]);
    let old_checkpoint = signed_checkpoint(
        VECTOR_DOMAIN,
        old_entries,
        Arc::new(site.release_key.note_signer(&log_origin(VECTOR_DOMAIN))?),
    )
    .await?;
    let inclusion = inclusion_proof(&leaves, 0).map_err(|e| SigningError::Key(e.to_string()))?;
    let consistency =
        consistency_proof(&leaves, 1).map_err(|e| SigningError::Key(e.to_string()))?;
    let base = json!({
        "description": "The site log over two releases: entries, leaf hashes, tiles, the signed checkpoint, an inclusion proof and a consistency proof (AWP 2.0 §14.5).",
        "domain": VECTOR_DOMAIN,
        "origin": log_origin(VECTOR_DOMAIN),
        "releaseKid": site.release_key.kid(),
        "releasePublicKey": site.release_key.public_key(),
        "releaseSeed": b64(&RELEASE_SEED),
        "manifests": site.manifests,
        "entries": entries,
        "entryJcs": entries.iter().map(|e| entry_bytes(e).map(|b| b64(&b))).collect::<Result<Vec<_>, _>>()?,
        "leafHashes": leaves.iter().map(|h| b64(&h.0)).collect::<Vec<_>>(),
        "rootHash": STANDARD.encode(tree_root.0),
        "tiles": tiles,
        "checkpoint": checkpoint,
        "previousCheckpoint": old_checkpoint,
        "previousRootHash": STANDARD.encode(old_root.0),
        "inclusionProof": { "index": 0, "size": leaves.len(), "hashes": inclusion.iter().map(|h| b64(&h.0)).collect::<Vec<_>>() },
        "consistencyProof": { "oldSize": 1, "newSize": leaves.len(), "hashes": consistency.iter().map(|h| b64(&h.0)).collect::<Vec<_>>() },
        "expected": valid(),
    });
    cases.insert("release-checkpoint/valid.json".to_owned(), base.clone());

    // A checkpoint for a log that rewrote release 1.
    let mut rewritten = entries.clone();
    rewritten[0].manifest = awp_types::Digest::from_sha256_bytes([0xee; 32]);
    let forked = signed_checkpoint(
        VECTOR_DOMAIN,
        &rewritten,
        Arc::new(site.release_key.note_signer(&log_origin(VECTOR_DOMAIN))?),
    )
    .await?;
    let mut case_fork = base.clone();
    merge(
        &mut case_fork,
        json!({
            "description": "A checkpoint whose tree rewrote release 1, so it does not extend the previous checkpoint.",
            "checkpoint": forked,
            "entries": rewritten,
            "expected": invalid("log.consistency"),
        }),
    );
    cases.insert("release-checkpoint/non-extending.json".to_owned(), case_fork);

    let mut other_digest = entries.clone();
    other_digest[1].manifest = awp_types::Digest::from_sha256_bytes([0xab; 32]);
    let other = signed_checkpoint(
        VECTOR_DOMAIN,
        &other_digest,
        Arc::new(site.release_key.note_signer(&log_origin(VECTOR_DOMAIN))?),
    )
    .await?;
    let mut case_entry = base.clone();
    merge(
        &mut case_entry,
        json!({
            "description": "A log whose entry for release 2 names another manifest digest; the checkpoint over it extends the previous one.",
            "checkpoint": other,
            "entries": other_digest,
            "expected": invalid("release.inclusion"),
        }),
    );
    cases.insert("release-checkpoint/entry-other-digest.json".to_owned(), case_entry);

    let wrong_key = signed_checkpoint(
        VECTOR_DOMAIN,
        entries,
        Arc::new(site.edge_key.note_signer(&log_origin(VECTOR_DOMAIN))?),
    )
    .await?;
    let mut case_key = base.clone();
    merge(
        &mut case_key,
        json!({
            "description": "The checkpoint signed by the edge's `card`/`receipt` key instead of a `release` key.",
            "checkpoint": wrong_key,
            "expected": invalid("checkpoint.signature"),
        }),
    );
    cases.insert("release-checkpoint/non-release-key.json".to_owned(), case_key);

    let body = CheckpointBody {
        origin: "example.org/.well-known/awp/log".to_owned(),
        size: leaves.len() as u64,
        root_hash: tree_root,
    };
    let wrong_origin = adk_auth::signed_audit::sign_note(
        &body.to_text(),
        &[Arc::new(site.release_key.note_signer("example.org/.well-known/awp/log")?)],
    )
    .await
    .map_err(|e| SigningError::Key(e.to_string()))?;
    let mut case_origin = base;
    merge(
        &mut case_origin,
        json!({
            "description": "A checkpoint whose origin line names another site's log.",
            "checkpoint": wrong_origin,
            "expected": invalid("checkpoint.signature"),
        }),
    );
    cases.insert("release-checkpoint/wrong-origin.json".to_owned(), case_origin);
    Ok(())
}

fn parse<T: serde::de::DeserializeOwned>(case: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(case.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("{key}: {e}"))
}

/// Evaluates one case with this crate's verifier code and returns `None` for a valid case or
/// the failure name; the result must match the case's `expected`.
///
/// # Errors
///
/// A message when the case is malformed.
pub fn check_case(vector: &str, case: &Value) -> Result<Option<&'static str>, String> {
    match vector {
        "signed-contract" => {
            let document: SiteContract = parse(case, "document")?;
            let root_kid: String = parse(case, "rootKid")?;
            let root: PublicKeyJwk = parse(case, "rootPublicKey")?;
            let record: String = case
                .pointer("/dnsRecord/value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if RootRecord::parse(&record).is_none_or(|record| record.jkt != jwk_thumbprint(&root)) {
                return Ok(Some("root.unanchored"));
            }
            let proof = document.proof.clone().ok_or("the document has no proof")?;
            if proof.kid != root_kid {
                return Ok(Some("contract.signature"));
            }
            Ok(verify(&document, &proof, &root).err().map(|_| "contract.signature"))
        }
        "signed-delegation" => {
            let delegation: Delegation = parse(case, "document")?;
            let root: PublicKeyJwk = parse(case, "rootPublicKey")?;
            let root_kid: String = parse(case, "rootKid")?;
            let revocations: Vec<Revocation> = parse(case, "revocations")?;
            let at: DateTime<Utc> = parse(case, "at")?;
            let keys = KeySet {
                root: awp_types::RootKey { kid: root_kid, public_key: root, endorsement: None },
                delegations: vec![delegation.clone()],
                retired_roots: vec![],
                revocations,
            };
            if verify_key_set(&keys).is_err() {
                return Ok(Some("delegation.signature"));
            }
            Ok(match delegated_key(&keys, &delegation.kid, KeyScope::Release, Dated::Durable(at)) {
                Ok(_) => None,
                Err(crate::signing::keys::KeyError::Revoked(_)) => Some("key.revoked"),
                Err(_) => Some("key.window"),
            })
        }
        "signed-agent-card" => {
            let card: Value = parse(case, "document")?;
            let keys = KeySet {
                root: awp_types::RootKey {
                    kid: parse(case, "rootKid")?,
                    public_key: parse(case, "rootPublicKey")?,
                    endorsement: None,
                },
                delegations: parse(case, "delegations")?,
                retired_roots: vec![],
                revocations: vec![],
            };
            let at: DateTime<Utc> = parse(case, "at")?;
            // A delegation whose root proof fails is not a valid card key either.
            let keys = KeySet {
                delegations: keys
                    .delegations
                    .iter()
                    .filter(|d| verify(*d, &d.proof, &keys.root.public_key).is_ok())
                    .cloned()
                    .collect(),
                ..keys.clone()
            };
            Ok(verify_agent_card(&card, &keys, at).err().map(|_| "card.signature"))
        }
        "release-checkpoint" => {
            let domain: String = parse(case, "domain")?;
            let release: PublicKeyJwk = parse(case, "releasePublicKey")?;
            let checkpoint: String = parse(case, "checkpoint")?;
            let entries: Vec<LogEntry> = parse(case, "entries")?;
            let manifests: Vec<awp_types::ReleaseManifest> = parse(case, "manifests")?;
            let raw = public_key_bytes(&release).map_err(|e| e.to_string())?;
            let verifier = checkpoint_verifier(&domain, &raw).map_err(|e| e.to_string())?;
            let note = Note::parse(&checkpoint).map_err(|e| e.to_string())?;
            if note.verified_by(std::slice::from_ref(&verifier)).is_empty() {
                return Ok(Some("checkpoint.signature"));
            }
            let body = CheckpointBody::parse(&note.text).map_err(|e| e.to_string())?;
            if body.origin != log_origin(&domain) {
                return Ok(Some("checkpoint.signature"));
            }
            let leaves: Vec<AuditHash> = entries
                .iter()
                .map(|e| entry_bytes(e).map(|b| leaf_hash(&b)))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let previous_root: String = parse(case, "previousRootHash")?;
            let previous: [u8; 32] = STANDARD
                .decode(previous_root)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or("previousRootHash is not 32 bytes")?;
            let previous = AuditHash(previous);
            let consistency = consistency_proof(&leaves, 1).map_err(|e| e.to_string())?;
            if root(&leaves) != body.root_hash
                || verify_consistency(1, body.size, &previous, &body.root_hash, &consistency)
                    .is_err()
            {
                return Ok(Some("log.consistency"));
            }
            for manifest in &manifests {
                let digest =
                    crate::signing::jws::document_digest(manifest).map_err(|e| e.to_string())?;
                let leaf = entry_bytes(&LogEntry { seq: manifest.seq, manifest: digest })
                    .map(|b| leaf_hash(&b))
                    .map_err(|e| e.to_string())?;
                let index = manifest.seq.saturating_sub(1);
                let proof = inclusion_proof(&leaves, index).map_err(|e| e.to_string())?;
                if verify_inclusion(index, body.size, &leaf, &proof, &body.root_hash).is_err() {
                    return Ok(Some("release.inclusion"));
                }
            }
            Ok(None)
        }
        other => Err(format!("unknown vector `{other}`")),
    }
}
