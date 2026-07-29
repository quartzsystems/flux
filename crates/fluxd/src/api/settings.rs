//! Appliance settings: TLS, retention, identity, and configuration transfer.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Router;
use flux_core::types::Id;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{AdminAuth, Json};
use crate::state::AppState;
use crate::store::models::Setting;
use crate::store::{flows, port_groups, profiles, settings, tests as test_store};
use crate::tls;

/// Mounts the settings routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/export", get(export))
        .route("/import", post(import))
        .route("/tls", post(upload_tls).delete(remove_tls))
        .route("/{key}", get(get_one).put(put_setting))
}

/// Every setting.
async fn list(State(state): State<AppState>, _auth: AdminAuth) -> ApiResult<Json<Vec<Setting>>> {
    Ok(Json(settings::list(state.store.pool()).await?))
}

/// One setting.
async fn get_one(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(key): Path<String>,
) -> ApiResult<Json<Setting>> {
    settings::get(state.store.pool(), &key)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("setting {key}")))
}

/// Writes a setting.
#[tracing::instrument(skip(state, value), fields(%key))]
async fn put_setting(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Path(key): Path<String>,
    Json(value): Json<serde_json::Value>,
) -> ApiResult<Json<Setting>> {
    if key.trim().is_empty() || key.len() > 64 {
        return Err(ApiError::field("key", "must be between 1 and 64 characters"));
    }

    // TLS is managed through its own endpoint, which validates the material and
    // writes the files. Letting it through here would allow a settings row that
    // claims TLS is on with no certificate behind it.
    if key == "tls" {
        return Err(ApiError::Conflict(
            "upload a certificate to /settings/tls rather than editing this key".into(),
        ));
    }

    let setting = settings::put(state.store.pool(), &key, &value, Some(actor.user_id)).await?;
    tracing::info!(actor = %actor.username, "setting updated");
    Ok(Json(setting))
}

// ---------------------------------------------------------------------------
// TLS
// ---------------------------------------------------------------------------

/// A certificate and its private key.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TlsUpload {
    /// PEM-encoded certificate chain, leaf first.
    pub certificate: String,
    /// PEM-encoded private key.
    pub private_key: String,
}

/// Installs a certificate.
///
/// The material is parsed and checked before anything is written: an appliance
/// that accepted a broken certificate and then failed to start its listener
/// would be unreachable, which is the one failure mode this endpoint must not
/// produce.
///
/// Taking effect needs a restart. Rebinding the listener under a live server is
/// possible but would mean every in-flight request — including this one — being
/// dropped mid-response, and a restart is both clearer and what an operator
/// expects after installing a certificate.
#[tracing::instrument(skip_all)]
async fn upload_tls(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Json(body): Json<TlsUpload>,
) -> ApiResult<Json<serde_json::Value>> {
    let material = tls::Material::parse(&body.certificate, &body.private_key)
        .map_err(|e| ApiError::field("certificate", e.to_string()))?;

    let paths = tls::install(&state.config.tls_dir, &body.certificate, &body.private_key)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("installing the certificate: {e}")))?;

    let value = serde_json::json!({
        "enabled": true,
        "certPath": paths.certificate.display().to_string(),
        "keyPath": paths.private_key.display().to_string(),
        "subject": material.subject,
        "notAfter": material.not_after,
    });
    settings::put(state.store.pool(), "tls", &value, Some(actor.user_id)).await?;

    tracing::warn!(
        actor = %actor.username,
        subject = %material.subject,
        "a TLS certificate was installed; restart fluxd for it to take effect"
    );

    Ok(Json(serde_json::json!({
        "installed": true,
        "subject": material.subject,
        "notAfter": material.not_after,
        "restartRequired": true,
    })))
}

/// Removes the certificate and returns the appliance to plain HTTP.
#[tracing::instrument(skip(state))]
async fn remove_tls(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
) -> ApiResult<Json<serde_json::Value>> {
    tls::remove(&state.config.tls_dir)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("removing the certificate: {e}")))?;

    let value = serde_json::json!({
        "enabled": false,
        "certPath": null,
        "keyPath": null,
        "subject": null,
        "notAfter": null,
    });
    settings::put(state.store.pool(), "tls", &value, Some(actor.user_id)).await?;

    tracing::warn!(actor = %actor.username, "the TLS certificate was removed");
    Ok(Json(serde_json::json!({ "removed": true, "restartRequired": true })))
}

// ---------------------------------------------------------------------------
// Configuration transfer
// ---------------------------------------------------------------------------

/// Format version of the export bundle.
///
/// Bumped whenever the shape changes incompatibly, so an import can refuse a
/// bundle it would misread rather than silently dropping half of it.
pub const BUNDLE_VERSION: u32 = 1;

/// A portable snapshot of an appliance's configuration.
///
/// Deliberately excludes users, sessions, runs, and results. Configuration is
/// what an operator wants to move between appliances or keep in version
/// control; credentials and history are neither portable nor safe to copy.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigBundle {
    /// Bundle format version.
    pub version: u32,
    /// The daemon that produced it.
    pub exported_by: String,
    /// When, as an RFC 3339 timestamp.
    pub exported_at: String,
    /// Port groups, with their member ports named by PCI address.
    pub port_groups: Vec<serde_json::Value>,
    /// Flow definitions.
    pub flows: Vec<NamedConfig>,
    /// Load profile definitions.
    pub load_profiles: Vec<NamedConfig>,
    /// Test definitions, referencing flows and profiles by name.
    pub tests: Vec<serde_json::Value>,
    /// Appliance settings, excluding TLS.
    pub settings: Vec<Setting>,
}

/// A named configuration document.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedConfig {
    /// The object's name, which is what an import matches on.
    pub name: String,
    /// Its configuration document.
    pub config: serde_json::Value,
}

/// Exports the appliance's configuration.
///
/// References travel by name rather than by id: identifiers are per-appliance,
/// and a bundle that carried them would import as a set of dangling pointers.
#[tracing::instrument(skip(state))]
async fn export(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
) -> ApiResult<Json<ConfigBundle>> {
    let pool = state.store.pool();

    let flow_rows = flows::list(pool).await?;
    let profile_rows = profiles::list(pool).await?;
    let test_rows = test_store::list(pool).await?;
    let group_rows = port_groups::list(pool).await?;

    let flows_by_id: std::collections::HashMap<Id, String> =
        flow_rows.iter().map(|f| (f.id, f.name.clone())).collect();
    let profiles_by_id: std::collections::HashMap<Id, String> =
        profile_rows.iter().map(|p| (p.id, p.name.clone())).collect();

    let mut groups = Vec::with_capacity(group_rows.len());
    for group in &group_rows {
        let member_ids = port_groups::member_ids(pool, group.id).await?;
        let members = crate::store::ports::get_many_ordered(pool, &member_ids).await?;

        groups.push(serde_json::json!({
            "name": group.name,
            "engineMode": group.engine_mode.as_str(),
            "trexCfg": group.trex_cfg,
            // Ports travel as PCI addresses: the hardware identity is the only
            // thing meaningful on another appliance.
            "portPciAddrs": members.iter().map(|p| p.pci_addr.as_str()).collect::<Vec<_>>(),
        }));
    }

    let tests = test_rows
        .iter()
        .map(|test| {
            serde_json::json!({
                "name": test.name,
                "type": test.test_type.as_str(),
                "config": test.config,
                "flowNames": test.flow_ids.iter().filter_map(|id| flows_by_id.get(id)).collect::<Vec<_>>(),
                "profileNames": test.profile_ids.iter().filter_map(|id| profiles_by_id.get(id)).collect::<Vec<_>>(),
            })
        })
        .collect();

    // TLS is excluded: the private key is on this appliance and belongs to this
    // appliance's hostname.
    let settings = settings::list(pool).await?.into_iter().filter(|s| s.key != "tls").collect();

    tracing::info!(
        actor = %actor.username,
        flows = flow_rows.len(),
        profiles = profile_rows.len(),
        tests = test_rows.len(),
        "configuration exported"
    );

    Ok(Json(ConfigBundle {
        version: BUNDLE_VERSION,
        exported_by: format!("flux {}", super::system::VERSION),
        exported_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        port_groups: groups,
        flows: flow_rows
            .into_iter()
            .map(|f| NamedConfig { name: f.name, config: f.config })
            .collect(),
        load_profiles: profile_rows
            .into_iter()
            .map(|p| NamedConfig { name: p.name, config: p.config })
            .collect(),
        tests,
        settings,
    }))
}

/// What an import did.
#[derive(Debug, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    /// Flows created.
    pub flows_created: usize,
    /// Flows that already existed and were left alone.
    pub flows_skipped: usize,
    /// Load profiles created.
    pub profiles_created: usize,
    /// Load profiles skipped.
    pub profiles_skipped: usize,
    /// Tests created.
    pub tests_created: usize,
    /// Tests skipped.
    pub tests_skipped: usize,
    /// Anything that could not be imported, and why.
    pub problems: Vec<String>,
}

/// Imports a configuration bundle.
///
/// Additive and idempotent: an object whose name already exists is left alone
/// rather than overwritten. Overwriting would silently discard whatever the
/// operator had configured under that name, which is not something an import
/// should be able to do without being asked.
///
/// Port groups are not imported. They name physical hardware, and a bundle from
/// another appliance describes NICs this one does not have.
#[tracing::instrument(skip_all)]
async fn import(
    State(state): State<AppState>,
    AdminAuth(actor): AdminAuth,
    Json(bundle): Json<ConfigBundle>,
) -> ApiResult<Json<ImportSummary>> {
    if bundle.version != BUNDLE_VERSION {
        return Err(ApiError::field(
            "version",
            format!(
                "this bundle is version {}; this appliance reads version {BUNDLE_VERSION}",
                bundle.version
            ),
        ));
    }

    let pool = state.store.pool();
    let mut summary = ImportSummary::default();

    let existing_flows: std::collections::HashSet<String> =
        flows::list(pool).await?.into_iter().map(|f| f.name).collect();
    let existing_profiles: std::collections::HashSet<String> =
        profiles::list(pool).await?.into_iter().map(|p| p.name).collect();
    let existing_tests: std::collections::HashSet<String> =
        test_store::list(pool).await?.into_iter().map(|t| t.name).collect();

    for flow in &bundle.flows {
        if existing_flows.contains(&flow.name) {
            summary.flows_skipped += 1;
            continue;
        }
        // The document is stored as given; it is validated on the next edit or
        // run, and refusing a whole bundle for one unreadable flow would be
        // worse than importing it for the operator to fix.
        match flows::create(pool, &flow.name, &flow.config, Some(actor.user_id)).await {
            Ok(_) => summary.flows_created += 1,
            Err(err) => summary.problems.push(format!("flow {}: {err}", flow.name)),
        }
    }

    for profile in &bundle.load_profiles {
        if existing_profiles.contains(&profile.name) {
            summary.profiles_skipped += 1;
            continue;
        }
        match profiles::create(pool, &profile.name, &profile.config, Some(actor.user_id)).await {
            Ok(_) => summary.profiles_created += 1,
            Err(err) => summary.problems.push(format!("profile {}: {err}", profile.name)),
        }
    }

    // Tests are last: they reference flows and profiles by name, which have to
    // exist by now.
    let flows_by_name: std::collections::HashMap<String, Id> =
        flows::list(pool).await?.into_iter().map(|f| (f.name, f.id)).collect();
    let profiles_by_name: std::collections::HashMap<String, Id> =
        profiles::list(pool).await?.into_iter().map(|p| (p.name, p.id)).collect();

    for test in &bundle.tests {
        let Some(name) = test.get("name").and_then(|v| v.as_str()) else {
            summary.problems.push("a test in the bundle has no name".into());
            continue;
        };
        if existing_tests.contains(name) {
            summary.tests_skipped += 1;
            continue;
        }

        let test_type = match test
            .get("type")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<flux_core::types::TestType>().ok())
        {
            Some(t) => t,
            None => {
                summary.problems.push(format!("test {name}: unrecognised type"));
                continue;
            }
        };

        let resolve = |key: &str, index: &std::collections::HashMap<String, Id>| -> Vec<Id> {
            test.get(key)
                .and_then(|v| v.as_array())
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|n| n.as_str())
                        .filter_map(|n| index.get(n).copied())
                        .collect()
                })
                .unwrap_or_default()
        };

        let flow_ids = resolve("flowNames", &flows_by_name);
        let profile_ids = resolve("profileNames", &profiles_by_name);

        if flow_ids.is_empty() && profile_ids.is_empty() {
            summary
                .problems
                .push(format!("test {name}: none of its flows or profiles are present"));
            continue;
        }

        let config = test.get("config").cloned().unwrap_or_else(|| serde_json::json!({}));
        match test_store::create(
            pool,
            name,
            test_type,
            &config,
            &flow_ids,
            &profile_ids,
            Some(actor.user_id),
        )
        .await
        {
            Ok(_) => summary.tests_created += 1,
            Err(err) => summary.problems.push(format!("test {name}: {err}")),
        }
    }

    for setting in &bundle.settings {
        if setting.key == "tls" {
            continue;
        }
        settings::put(pool, &setting.key, &setting.value, Some(actor.user_id)).await?;
    }

    tracing::info!(
        actor = %actor.username,
        created = summary.flows_created + summary.profiles_created + summary.tests_created,
        problems = summary.problems.len(),
        "configuration imported"
    );

    Ok(Json(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_round_trips_through_json() {
        let bundle = ConfigBundle {
            version: BUNDLE_VERSION,
            exported_by: "flux 0.1.0".into(),
            exported_at: "2026-01-01T00:00:00Z".into(),
            port_groups: vec![serde_json::json!({ "name": "pair-a" })],
            flows: vec![NamedConfig {
                name: "udp-64".into(),
                config: serde_json::json!({ "size": { "type": "fixed", "bytes": 64 } }),
            }],
            load_profiles: Vec::new(),
            tests: vec![serde_json::json!({ "name": "t", "flowNames": ["udp-64"] })],
            settings: Vec::new(),
        };

        let json = serde_json::to_string(&bundle).unwrap();
        let back: ConfigBundle = serde_json::from_str(&json).unwrap();

        assert_eq!(back.version, BUNDLE_VERSION);
        assert_eq!(back.flows[0].name, "udp-64");
        assert_eq!(back.tests[0]["flowNames"][0], "udp-64");
    }

    #[test]
    fn a_bundle_references_by_name_rather_than_by_identifier() {
        // Identifiers are per-appliance; a bundle carrying them would import as
        // a set of dangling pointers.
        let json = serde_json::to_string(&ConfigBundle {
            version: BUNDLE_VERSION,
            exported_by: String::new(),
            exported_at: String::new(),
            port_groups: Vec::new(),
            flows: vec![NamedConfig { name: "f".into(), config: serde_json::json!({}) }],
            load_profiles: Vec::new(),
            tests: Vec::new(),
            settings: Vec::new(),
        })
        .unwrap();

        assert!(!json.contains("\"id\""), "the bundle should carry no identifiers: {json}");
    }

    #[test]
    fn an_import_summary_starts_empty() {
        let summary = ImportSummary::default();
        assert_eq!(summary.flows_created, 0);
        assert!(summary.problems.is_empty());
    }
}
