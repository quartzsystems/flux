//! The device under test.
//!
//! Everything else the topology view draws — ports, groups, flows — already has
//! an endpoint. The one thing that has nowhere else to live is the operator's
//! description of the box in the middle, which is not derivable from anything
//! Flux can see on the wire.
//!
//! It lives in the settings table but has its own route because its audience is
//! different: settings are administrative, whereas the device under test is
//! test metadata that every viewer needs to read and every operator needs to
//! edit. Reaching it through the admin-only settings routes would mean an
//! operator could start a run recording a device they are not allowed to name.

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{Auth, Json, OperatorAuth};
use crate::state::AppState;
use crate::store::settings;

/// Most entries a description may carry.
const MAX_ENTRIES: usize = 16;

/// Longest field name.
const MAX_KEY_LEN: usize = 48;

/// Longest field value.
const MAX_VALUE_LEN: usize = 256;

/// Mounts the topology routes.
pub fn router() -> Router<AppState> {
    Router::new().route("/dut", get(get_dut).put(put_dut))
}

/// An operator's description of the device under test.
///
/// Free-form key/value pairs rather than a fixed struct: what identifies a
/// device varies by what it is. A switch is a vendor and a firmware revision, a
/// virtual appliance is an image digest, and a fixed schema would force one of
/// them to be recorded in a field named for the other.
///
/// The report prints whatever is here verbatim, so absent fields are absent
/// from the map rather than present and null.
#[derive(Debug, Default, Clone, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct Dut(pub BTreeMap<String, String>);

impl Dut {
    /// Checks the description and drops empty entries.
    ///
    /// Trimming to nothing is treated as clearing the field, which is what an
    /// operator emptying an input means.
    fn sanitise(self) -> Result<Self, ApiError> {
        let mut out = BTreeMap::new();

        for (key, value) in self.0 {
            let key = key.trim().to_string();
            let value = value.trim().to_string();

            if key.is_empty() || value.is_empty() {
                continue;
            }
            if key.chars().count() > MAX_KEY_LEN {
                return Err(ApiError::field(
                    "dut",
                    format!("the field name {key:?} is longer than {MAX_KEY_LEN} characters"),
                ));
            }
            if value.chars().count() > MAX_VALUE_LEN {
                return Err(ApiError::field(
                    "dut",
                    format!("the value of {key:?} is longer than {MAX_VALUE_LEN} characters"),
                ));
            }

            out.insert(key, value);
        }

        if out.len() > MAX_ENTRIES {
            return Err(ApiError::field(
                "dut",
                format!("a description may carry at most {MAX_ENTRIES} fields"),
            ));
        }

        Ok(Self(out))
    }
}

/// The recorded device under test.
async fn get_dut(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Dut>> {
    let stored = settings::get(state.store.pool(), settings::DUT_KEY).await?;

    // A description written by an older build, or edited by hand, may not parse.
    // Reporting nothing is better than a 500 on a page whose other half is fine.
    let dut = stored.and_then(|s| serde_json::from_value::<Dut>(s.value).ok()).unwrap_or_default();

    Ok(Json(dut))
}

/// Records the device under test.
#[tracing::instrument(skip_all)]
async fn put_dut(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Json(dut): Json<Dut>,
) -> ApiResult<Json<Dut>> {
    let dut = dut.sanitise()?;

    let value = serde_json::to_value(&dut)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("serialising the description: {e}")))?;
    settings::put(state.store.pool(), settings::DUT_KEY, &value, Some(actor.user_id)).await?;

    tracing::info!(actor = %actor.username, fields = dut.0.len(), "device under test recorded");
    Ok(Json(dut))
}

#[cfg(test)]
mod unit {
    use super::*;

    fn dut(pairs: &[(&str, &str)]) -> Dut {
        Dut(pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect())
    }

    #[test]
    fn a_plain_description_survives_unchanged() {
        let out = dut(&[("vendor", "Acme"), ("model", "AR-9000")])
            .sanitise()
            .expect("a plain description is valid");

        assert_eq!(out.0.get("vendor").map(String::as_str), Some("Acme"));
        assert_eq!(out.0.get("model").map(String::as_str), Some("AR-9000"));
    }

    #[test]
    fn an_emptied_field_is_dropped_rather_than_recorded_blank() {
        let out = dut(&[("vendor", "Acme"), ("firmware", "   ")])
            .sanitise()
            .expect("clearing a field is valid");

        assert_eq!(out.0.len(), 1);
        assert!(!out.0.contains_key("firmware"));
    }

    #[test]
    fn surrounding_whitespace_is_not_recorded() {
        let out = dut(&[("  vendor  ", "  Acme  ")]).sanitise().expect("padded input is valid");

        assert_eq!(out.0.get("vendor").map(String::as_str), Some("Acme"));
    }

    #[test]
    fn an_overlong_value_is_refused() {
        let long = "x".repeat(MAX_VALUE_LEN + 1);
        let err = dut(&[("model", &long)]).sanitise().expect_err("an overlong value is refused");

        assert!(matches!(err, ApiError::Validation(_)), "got {err:?}");
    }

    #[test]
    fn an_overlong_field_name_is_refused() {
        let long = "k".repeat(MAX_KEY_LEN + 1);
        assert!(dut(&[(&long, "value")]).sanitise().is_err());
    }

    #[test]
    fn too_many_fields_are_refused() {
        let owned: Vec<(String, String)> =
            (0..=MAX_ENTRIES).map(|i| (format!("field{i}"), "value".to_string())).collect();
        let borrowed: Vec<(&str, &str)> =
            owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        assert!(dut(&borrowed).sanitise().is_err());
    }

    #[test]
    fn the_wire_form_is_a_flat_object_the_report_can_print() {
        let json = serde_json::to_value(dut(&[("vendor", "Acme")])).expect("serialises");
        assert_eq!(json, serde_json::json!({ "vendor": "Acme" }));
    }
}
