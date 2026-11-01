//! The charter as the approval service's directory (spec 3.5 §7.6, A-I23): who holds which
//! role is what the charter in force says, never what a surface asserts.

use std::sync::Arc;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{AdkError, ApprovalPolicy, ApprovalRequest, ApproverDirectory, ApproverSelector};
use async_trait::async_trait;
use awp_types::{ActionClass, Governance, Role};
use uuid::Uuid;

use super::store::{CharterStore, is_agent_principal};

/// The approver group of `role`: `role:{role}`, such as `role:publisher`.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::{group_role, role_group};
/// use awp_types::Role;
///
/// assert_eq!(role_group(Role::Publisher), "role:publisher");
/// assert_eq!(group_role("role:publisher"), Some(Role::Publisher));
/// ```
pub fn role_group(role: Role) -> String {
    let name = serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    format!("role:{name}")
}

/// The role a `role:{role}` group names; `None` for any other group.
pub fn group_role(group: &str) -> Option<Role> {
    let name = group.strip_prefix("role:")?;
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

/// The approval policy of `action`'s chain under `governance` (spec 3.5 §7.6, A-I23): one
/// slot per role of the chain, `role:{role}`, each filled by a distinct person who holds the
/// role, none of them the requester. A charter that names no chain for `action` needs one
/// owner, as [`check_chain`](super::check_chain) does.
///
/// # Example
///
/// ```rust
/// use adk_core::ApproverSelector;
/// use adk_kernel::charter::chain_policy;
/// use awp_types::{ActionClass, ApprovalChain, Governance, Role};
///
/// let governance = Governance {
///     roles: Vec::new(),
///     approval_chains: vec![ApprovalChain {
///         action: ActionClass::Publish,
///         roles: vec![Role::Editor, Role::Publisher],
///     }],
/// };
/// let policy = chain_policy(&governance, &ActionClass::Publish);
/// assert_eq!(
///     policy.slots,
///     [ApproverSelector::group("role:editor"), ApproverSelector::group("role:publisher")]
/// );
/// assert_eq!(policy.quorum, 2);
/// assert!(policy.forbid_self_approval);
/// let charter = chain_policy(&governance, &ActionClass::CharterChange);
/// assert_eq!(charter.slots, [ApproverSelector::group("role:owner")]);
/// ```
pub fn chain_policy(governance: &Governance, action: &ActionClass) -> ApprovalPolicy {
    let roles = governance
        .approval_chains
        .iter()
        .find(|chain| &chain.action == action)
        .map_or_else(|| vec![Role::Owner], |chain| chain.roles.clone());
    ApprovalPolicy::for_slots(
        roles.into_iter().map(|role| ApproverSelector::group(role_group(role))),
    )
}

/// The approval service's [`ApproverDirectory`] over the site charters in a
/// [`CharterStore`] (spec 3.5 §7.6, A-I23): a person's groups are `role:{role}` for every
/// role their site's charter in force gives them, so a release chain, a charter signature or
/// a promotion is decided only by the people the charter names, whichever surface they
/// decide on. An agent holds no role. The directory governs the `role:` groups only: a
/// caller's claim to one is ignored, while other groups, such as an operator scope, stay as
/// the caller's authentication asserted them.
///
/// A request names its site in `args_redacted.site`; one that names none belongs to the
/// site set with [`for_site`](Self::for_site), so one directory serves every site of an
/// agency's kernel.
///
/// | Request | `role:` groups come from |
/// |---|---|
/// | its site has a charter in force | that charter's governance |
/// | no charter in force, and the request carries the charter it would commit (`args_redacted.charter`) | that charter's governance: the first charter's own signers sign it |
/// | otherwise, or no site | none |
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{ApproverDirectory, InMemoryApprovalStore, StoreApprovalService};
/// use adk_kernel::charter::{CharterDirectory, InMemoryCharterStore};
///
/// let site = uuid::Uuid::new_v4();
/// let directory = CharterDirectory::new(Arc::new(InMemoryCharterStore::new())).for_site(site);
/// assert!(directory.governs("role:publisher"));
/// assert!(!directory.governs("operator"));
/// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
///     .with_directory(Arc::new(directory));
/// # let _ = service;
/// ```
pub struct CharterDirectory {
    charters: Arc<dyn CharterStore>,
    site: Option<Uuid>,
}

impl std::fmt::Debug for CharterDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CharterDirectory").field("site", &self.site).finish_non_exhaustive()
    }
}

impl CharterDirectory {
    /// The directory of every site whose charters `charters` keeps.
    pub fn new(charters: Arc<dyn CharterStore>) -> Self {
        Self { charters, site: None }
    }

    /// Reads a request that names no site as `site`'s.
    #[must_use]
    pub fn for_site(mut self, site: Uuid) -> Self {
        self.site = Some(site);
        self
    }
}

#[async_trait]
impl ApproverDirectory for CharterDirectory {
    async fn groups(
        &self,
        subject: &SubjectRef,
        request: &ApprovalRequest,
    ) -> adk_core::Result<Vec<String>> {
        if subject.kind != SubjectKind::Human || is_agent_principal(&subject.id) {
            return Ok(Vec::new());
        }
        let named = request.args_redacted.get("site").and_then(serde_json::Value::as_str);
        let site = match named {
            Some(named) => named.parse::<Uuid>().ok(),
            None => self.site,
        };
        let Some(site) = site else { return Ok(Vec::new()) };
        let current = self.charters.current(site).await.map_err(|error| {
            AdkError::new(
                adk_core::ErrorComponent::Agent,
                adk_core::ErrorCategory::Unavailable,
                "charter.directory.unavailable",
                format!("the charter store cannot say who holds which role: {error}"),
            )
        })?;
        let proposed = || request.args_redacted.get("charter")?.get("governance").cloned();
        let governance = match current {
            Some(version) => version.charter.governance,
            None => match proposed() {
                Some(proposed) => {
                    serde_json::from_value::<Governance>(proposed).unwrap_or_default()
                }
                None => return Ok(Vec::new()),
            },
        };
        Ok(governance
            .roles
            .iter()
            .filter(|holder| holder.principal == subject.id)
            .flat_map(|holder| holder.roles.iter().copied())
            .map(role_group)
            .collect())
    }

    fn governs(&self, group: &str) -> bool {
        group.starts_with("role:")
    }
}
