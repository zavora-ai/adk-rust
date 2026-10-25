//! Agency mode: one harness for many sites, each isolated (spec 3.5 §7.22, §7.17, A-I22).
//!
//! | Layer | Isolation |
//! |---|---|
//! | Kernel | Each site's two partitions carry the site as tenant and deny crossings; a request from one site's back office into another's — the coordinator's too — is denied and recorded as a crossing |
//! | Edge | Each site has its own edge, capabilities and stores; [`AgencyRouter`] sends a request only to the edge of the site its host names, and refuses and records any other |

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::{AdkError, Result};
use adk_harness::website::{SitePartitions, SiteRef};
use adk_kernel::{CrossingAccess, CrossingResource, CrossingRoute, PartitionPolicy, Partitions};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tower::ServiceExt;

use crate::audit;

/// The tool name under which a back-office agent's request reaches another site: any
/// read of another site's memory, drivers or sessions (spec 3.5 A-I22).
pub const CROSS_SITE_TOOL: &str = "cross_site_request";

/// The record of a request an agency refused at the edge (spec 3.5 §9.3).
pub const REQUEST_REFUSED: &str = "awp.request.refused";

/// The sites one harness runs (spec 3.5 A-I22).
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
/// use adk_kernel::Partitions;
/// use adk_website::deploy::Agency;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let mut agency = Agency::new(Partitions::in_memory());
/// agency.add(SiteRef::new(uuid::Uuid::new_v4(), "bakery", "https://bakery.example")).await.unwrap();
/// let again = agency.add(SiteRef::new(uuid::Uuid::new_v4(), "bakery", "https://other.example")).await;
/// assert!(again.is_err(), "no two sites share a name");
/// # }
/// ```
pub struct Agency {
    table: Partitions,
    sites: BTreeMap<String, (SiteRef, SitePartitions)>,
}

impl Agency {
    /// An agency over the kernel's partition `table`.
    pub fn new(table: Partitions) -> Self {
        Self { table, sites: BTreeMap::new() }
    }

    /// Adds `site` and installs its partitions; refuses a name or an id another site has.
    ///
    /// # Errors
    ///
    /// A duplicate site, or the partition table's error.
    pub async fn add(&mut self, site: SiteRef) -> Result<SitePartitions> {
        if self.sites.contains_key(&site.name) || self.sites.values().any(|(s, _)| s.id == site.id)
        {
            return Err(AdkError::config(format!(
                "the agency already runs a site named {} or with its id; every site needs its own (A-I22)",
                site.name
            )));
        }
        let partitions = SitePartitions::new(&site);
        partitions
            .install(&self.table, AuditActor::default())
            .await
            .map_err(|e| AdkError::config(e.to_string()))?;
        self.sites.insert(site.name.clone(), (site, partitions.clone()));
        Ok(partitions)
    }

    /// The sites, by name.
    pub fn sites(&self) -> impl Iterator<Item = &SiteRef> {
        self.sites.values().map(|(site, _)| site)
    }

    /// The partitions of site `name`.
    pub fn partitions(&self, name: &str) -> Option<&SitePartitions> {
        self.sites.get(name).map(|(_, partitions)| partitions)
    }

    /// The crossing policy of site `name`'s back office with `coordinator` as its
    /// coordinator, deciding [`CROSS_SITE_TOOL`] as a crossing into the partition its
    /// `partition` argument names (spec 3.5 A-I22): denied and recorded, since every site's
    /// partitions deny crossings.
    ///
    /// # Errors
    ///
    /// An unknown site.
    pub fn back_office_policy(&self, name: &str, coordinator: &str) -> Result<PartitionPolicy> {
        let partitions = self
            .partitions(name)
            .ok_or_else(|| AdkError::config(format!("the agency runs no site named {name}")))?;
        Ok(self
            .table
            .policy(partitions.back_office().id.clone())
            .with_coordinator(coordinator)
            .with_crossing_tool(
                CROSS_SITE_TOOL,
                CrossingRoute::argument(
                    CrossingResource::Memory,
                    CrossingAccess::Read,
                    "another site's memory",
                ),
            ))
    }

    /// The partition table.
    pub fn table(&self) -> &Partitions {
        &self.table
    }
}

/// Sends each request to the edge of the site its `Host` names; a request for any other host
/// is refused with `404` and recorded (spec 3.5 A-I22).
#[derive(Clone)]
pub struct AgencyRouter {
    edges: Arc<BTreeMap<String, axum::Router>>,
    audit: Option<AuditTrail>,
}

impl AgencyRouter {
    /// A router over each site's edge, by host.
    pub fn new(edges: BTreeMap<String, axum::Router>) -> Self {
        Self { edges: Arc::new(edges), audit: None }
    }

    /// Records refusals in `trail`.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// The router to serve.
    pub fn router(self) -> axum::Router {
        axum::Router::new().fallback(move |request: Request<Body>| {
            let agency = self.clone();
            async move { agency.dispatch(request).await }
        })
    }

    async fn dispatch(&self, request: Request<Body>) -> Response {
        // HTTP/2 carries the host in the URI's authority, HTTP/1.1 in `Host`.
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .or_else(|| request.uri().host())
            .map(|host| host.split(':').next().unwrap_or(host).to_ascii_lowercase())
            .unwrap_or_default();
        if let Some(edge) = self.edges.get(&host) {
            return match edge.clone().oneshot(request).await {
                Ok(response) => response,
                Err(never) => match never {},
            };
        }
        audit::record(
            self.audit.as_ref(),
            REQUEST_REFUSED,
            AuditActor::default(),
            json!({ "host": host, "path": request.uri().path(), "reason": "not_a_site_of_this_agency" }),
        )
        .await;
        tracing::info!(host, path = request.uri().path(), "awp.request.refused");
        (StatusCode::NOT_FOUND, axum::Json(json!({ "awp": "2.0", "error": { "code": "resource_not_found", "message": "no site is served at this host", "retryable": false } })))
            .into_response()
    }
}
