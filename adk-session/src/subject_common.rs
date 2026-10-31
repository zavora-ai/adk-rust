//! Encoding shared by the Redis, MongoDB, Neo4j, and Firestore subject registries and
//! consent stores.
//!
//! A subject is stored as flat fields — its JSON, pseudonym, state, the key and JSON of the
//! subject it is linked to, and microsecond timestamps — so every backend reads the same
//! record back. A consent record is stored as its JSON without `revokedAt`, with the grant
//! and revocation times beside it, so a revocation is a conditional write of one field.

use adk_core::Result;
use adk_core::principal::SubjectRef;
use adk_core::subject::{ConsentRecord, SubjectRecord, SubjectState, new_pseudonym};
use chrono::{DateTime, Utc};

use crate::durable_common::storage_error;

/// The error code of a subject registry or consent store whose backend fails.
pub(crate) const SUBJECT_STORAGE: &str = "session.subject_store.storage";

/// The key a subject is stored under: `kind:id`. Kinds contain no colon, so two subjects
/// never share a key.
pub(crate) fn subject_key(subject: &SubjectRef) -> String {
    subject.to_string()
}

/// The stored name of a state: `active` or `erasing`.
pub(crate) fn state_name(state: SubjectState) -> Result<String> {
    match serde_json::to_value(state) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        Ok(other) => Err(storage_error(SUBJECT_STORAGE, format!("state encodes as {other}"))),
        Err(e) => Err(storage_error(SUBJECT_STORAGE, format!("state does not encode: {e}"))),
    }
}

fn instant(field: &str, us: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(us).ok_or_else(|| {
        storage_error(SUBJECT_STORAGE, format!("stored field '{field}' is out of range: {us}"))
    })
}

fn json<T: serde::de::DeserializeOwned>(field: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| {
        storage_error(SUBJECT_STORAGE, format!("stored field '{field}' is corrupt: {e}"))
    })
}

/// Serializes `value` to JSON text.
pub(crate) fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| storage_error(SUBJECT_STORAGE, format!("serialize failed: {e}")))
}

/// A subject's stored fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredSubject {
    pub(crate) subject: String,
    pub(crate) pseudonym: String,
    pub(crate) state: String,
    pub(crate) linked_to_key: Option<String>,
    pub(crate) linked_to: Option<String>,
    pub(crate) linked_at_us: Option<i64>,
    pub(crate) created_at_us: i64,
}

impl StoredSubject {
    /// The fields of a newly registered `subject` in `state`, with a fresh pseudonym.
    pub(crate) fn fresh(
        subject: &SubjectRef,
        state: SubjectState,
        at: DateTime<Utc>,
    ) -> Result<Self> {
        Ok(Self {
            subject: to_json(subject)?,
            pseudonym: new_pseudonym(),
            state: state_name(state)?,
            linked_to_key: None,
            linked_to: None,
            linked_at_us: None,
            created_at_us: at.timestamp_micros(),
        })
    }

    /// Reads the fields back as a record.
    pub(crate) fn decode(&self) -> Result<SubjectRecord> {
        Ok(SubjectRecord {
            subject: json("subject", &self.subject)?,
            pseudonym: self.pseudonym.clone(),
            state: json("state", &format!("\"{}\"", self.state))?,
            linked_to: self.linked_to.as_deref().map(|text| json("linked_to", text)).transpose()?,
            linked_at: self.linked_at_us.map(|us| instant("linked_at_us", us)).transpose()?,
            created_at: instant("created_at_us", self.created_at_us)?,
        })
    }
}

/// A consent record's stored form: its JSON without `revokedAt`, the grant time, and the
/// revocation time.
pub(crate) fn consent_parts(record: &ConsentRecord) -> Result<(String, i64, Option<i64>)> {
    let unrevoked = ConsentRecord { revoked_at: None, ..record.clone() };
    Ok((
        to_json(&unrevoked)?,
        record.granted_at.timestamp_micros(),
        record.revoked_at.map(|at| at.timestamp_micros()),
    ))
}

/// Reads a consent record back from its stored JSON and revocation time.
pub(crate) fn consent_record(text: &str, revoked_at_us: Option<i64>) -> Result<ConsentRecord> {
    let mut record: ConsentRecord = json("record", text)?;
    record.revoked_at = revoked_at_us.map(|us| instant("revoked_at_us", us)).transpose()?;
    Ok(record)
}

/// Orders consent records oldest grant first, then by id.
pub(crate) fn sort_consent(records: &mut [ConsentRecord]) {
    records.sort_by(|a, b| a.granted_at.cmp(&b.granted_at).then_with(|| a.id.cmp(&b.id)));
}

#[cfg(test)]
mod tests {
    use adk_core::subject::ConsentPurpose;

    use super::*;

    #[test]
    fn subjects_and_consent_round_trip() {
        let at = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let visitor = SubjectRef::visitor("v:ü/1");
        let mut stored = StoredSubject::fresh(&visitor, SubjectState::Erasing, at).unwrap();
        stored.linked_to_key = Some(subject_key(&SubjectRef::human("alice")));
        stored.linked_to = Some(to_json(&SubjectRef::human("alice")).unwrap());
        stored.linked_at_us = Some(at.timestamp_micros());
        let record = stored.decode().unwrap();
        assert_eq!(record.subject, visitor);
        assert_eq!(record.state, SubjectState::Erasing);
        assert_eq!(record.linked_to, Some(SubjectRef::human("alice")));
        assert_eq!((record.created_at, record.linked_at), (at, Some(at)));

        let consent =
            ConsentRecord::new(visitor, ConsentPurpose::Analytics, "2026-10").granted_at(at);
        let revoked = ConsentRecord { revoked_at: Some(at), ..consent.clone() };
        let (text, granted, revoked_us) = consent_parts(&revoked).unwrap();
        assert_eq!((granted, revoked_us), (at.timestamp_micros(), Some(at.timestamp_micros())));
        assert_eq!(consent_record(&text, None).unwrap(), consent);
        assert_eq!(consent_record(&text, revoked_us).unwrap(), revoked);
    }
}
