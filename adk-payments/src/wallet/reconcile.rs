//! Reconciliation against providers (spec 3.5 §7.14).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::provider::{StatementEntry, StatementKind};
use super::store::{PaymentRecord, PaymentState};

/// One way the wallet's journal and a provider's statement disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Discrepancy {
    /// The wallet settled a payment the source does not show.
    MissingFromStatement {
        /// The wallet's payment id.
        payment_id: String,
        /// The source that should show it.
        source: String,
        /// The wallet's amount.
        amount: u64,
    },
    /// The source shows money moving that the wallet did not settle.
    NotInWallet {
        /// The statement line.
        entry_id: String,
        /// The source.
        source: String,
        /// The reference it carries, if any.
        reference: Option<String>,
        /// Its net amount.
        amount: u64,
    },
    /// Both show the payment at different amounts.
    AmountMismatch {
        /// The wallet's payment id.
        payment_id: String,
        /// The source.
        source: String,
        /// The wallet's amount.
        wallet_amount: u64,
        /// The source's net amount.
        statement_amount: i128,
    },
    /// A failed payment whose debit the provider did not return.
    OutstandingDebit {
        /// The wallet's payment id.
        payment_id: String,
        /// The debited amount.
        amount: u64,
    },
}

/// The result of reconciling a window (spec 3.5 §7.14).
///
/// # Example
///
/// ```
/// use adk_payments::wallet::ReconciliationReport;
///
/// let report = ReconciliationReport {
///     wallet: "site-1".into(),
///     from: chrono::Utc::now(),
///     to: chrono::Utc::now(),
///     sources: vec!["provider:treasury".into()],
///     payments: 0,
///     wallet_total: 0,
///     statement_total: 0,
///     matched: 0,
///     discrepancies: Vec::new(),
/// };
/// assert!(report.is_clean());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationReport {
    /// The wallet.
    pub wallet: String,
    /// Start of the window.
    pub from: DateTime<Utc>,
    /// End of the window, exclusive.
    pub to: DateTime<Utc>,
    /// The statements read: `provider:{name}` and each rail with its own statement.
    pub sources: Vec<String>,
    /// Settled payments in the window.
    pub payments: usize,
    /// What they total, in minor units.
    pub wallet_total: u64,
    /// The net the sources show for the window, in minor units.
    pub statement_total: i128,
    /// Payments both sides agree on.
    pub matched: usize,
    /// Where they disagree.
    pub discrepancies: Vec<Discrepancy>,
}

impl ReconciliationReport {
    /// Whether the journal and every statement agree.
    pub fn is_clean(&self) -> bool {
        self.discrepancies.is_empty()
    }
}

/// A statement keyed by source, as reconciliation reads it.
pub(crate) struct Statements {
    pub(crate) by_source: BTreeMap<String, Vec<StatementEntry>>,
}

/// Compares the settled and failed payments of a window with the sources' statements.
///
/// `source_of` names the statement a payment's money shows in. A statement reference that
/// names a payment settled outside the window (`known_elsewhere`) is not a discrepancy:
/// the debit and the settlement straddle the window's edge.
pub(crate) fn compare(
    wallet: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    payments: &[PaymentRecord],
    statements: &Statements,
    source_of: impl Fn(&PaymentRecord) -> String,
    known_elsewhere: &BTreeSet<String>,
) -> ReconciliationReport {
    let mut observed: BTreeMap<(String, String), (i128, String)> = BTreeMap::new();
    let mut discrepancies = Vec::new();
    let mut statement_total: i128 = 0;
    for (source, entries) in &statements.by_source {
        for entry in entries {
            let signed = match entry.kind {
                StatementKind::Debit => i128::from(entry.amount),
                StatementKind::Reversal => -i128::from(entry.amount),
                // The owner adding funds moves no money to a payee.
                StatementKind::Funding => continue,
            };
            statement_total += signed;
            match &entry.reference {
                Some(reference) => {
                    let slot = observed
                        .entry((source.clone(), reference.clone()))
                        .or_insert((0, entry.id.clone()));
                    slot.0 += signed;
                }
                None => discrepancies.push(Discrepancy::NotInWallet {
                    entry_id: entry.id.clone(),
                    source: source.clone(),
                    reference: None,
                    amount: entry.amount,
                }),
            }
        }
    }

    let settled: Vec<&PaymentRecord> =
        payments.iter().filter(|record| record.state == PaymentState::Settled).collect();
    let mut matched = 0;
    let mut wallet_total = 0u64;
    for record in &settled {
        let amount = record.settlement.as_ref().map_or(record.amount, |settled| settled.amount);
        wallet_total = wallet_total.saturating_add(amount);
        let source = source_of(record);
        match observed.remove(&(source.clone(), record.id.clone())) {
            Some((net, _)) if net == i128::from(amount) => matched += 1,
            Some((net, _)) => discrepancies.push(Discrepancy::AmountMismatch {
                payment_id: record.id.clone(),
                source,
                wallet_amount: amount,
                statement_amount: net,
            }),
            None => discrepancies.push(Discrepancy::MissingFromStatement {
                payment_id: record.id.clone(),
                source,
                amount,
            }),
        }
    }
    for record in payments {
        if record.state == PaymentState::Failed && record.debit_outstanding {
            let amount = record.debit.as_ref().map_or(record.amount, |debit| debit.amount);
            observed.remove(&(source_of(record), record.id.clone()));
            discrepancies
                .push(Discrepancy::OutstandingDebit { payment_id: record.id.clone(), amount });
        }
    }
    for ((source, reference), (net, entry_id)) in observed {
        if net == 0 || known_elsewhere.contains(&reference) {
            continue;
        }
        discrepancies.push(Discrepancy::NotInWallet {
            entry_id,
            source,
            reference: Some(reference),
            amount: u64::try_from(net.unsigned_abs()).unwrap_or(u64::MAX),
        });
    }

    ReconciliationReport {
        wallet: wallet.to_string(),
        from,
        to,
        sources: statements.by_source.keys().cloned().collect(),
        payments: settled.len(),
        wallet_total,
        statement_total,
        matched,
        discrepancies,
    }
}
