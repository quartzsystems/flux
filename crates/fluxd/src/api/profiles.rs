//! Load profile endpoints.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Router;
use flux_core::config::{Validate, Validation};
use flux_core::profile::LoadProfileConfig;
use flux_core::types::Id;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{Auth, Json, OperatorAuth};
use crate::orch::profile as translate;
use crate::state::AppState;
use crate::store::models::LoadProfile;
use crate::store::{is_unique_violation, ports, profiles};

/// Mounts the load profile routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/preview", post(preview))
        .route("/{id}", get(get_one).put(update).delete(delete))
}

/// Every profile.
async fn list(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<LoadProfile>>> {
    Ok(Json(profiles::list(state.store.pool()).await?))
}

/// One profile.
async fn get_one(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<Id>,
) -> ApiResult<Json<LoadProfile>> {
    profiles::get(state.store.pool(), id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("load profile {id}")))
}

/// The body for creating or replacing a profile.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInput {
    /// Operator-assigned label.
    pub name: String,
    /// The load definition.
    pub config: LoadProfileConfig,
}

impl ProfileInput {
    /// Checks the name and delegates the document to its own validation.
    fn validate(&self) -> ApiResult<()> {
        let mut v = Validation::new();

        let name = self.name.trim();
        v.require(!name.is_empty(), "name", "must not be empty");
        v.require(name.chars().count() <= 64, "name", "must be at most 64 characters");

        v.scope("config", |v| self.config.validate_into(v));
        v.finish()?;
        Ok(())
    }
}

/// Creates a profile.
#[tracing::instrument(skip_all, fields(name = %body.name))]
async fn create(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Json(body): Json<ProfileInput>,
) -> ApiResult<Json<LoadProfile>> {
    body.validate()?;
    check_ports(&state, &body.config).await?;

    let config = serde_json::to_value(&body.config).map_err(|e| ApiError::Internal(e.into()))?;
    let profile =
        profiles::create(state.store.pool(), body.name.trim(), &config, Some(actor.user_id))
            .await
            .map_err(name_conflict)?;

    tracing::info!(actor = %actor.username, profile_id = %profile.id, "load profile created");
    Ok(Json(profile))
}

/// Replaces a profile.
#[tracing::instrument(skip_all, fields(profile_id = %id))]
async fn update(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
    Json(body): Json<ProfileInput>,
) -> ApiResult<Json<LoadProfile>> {
    body.validate()?;
    check_ports(&state, &body.config).await?;

    let config = serde_json::to_value(&body.config).map_err(|e| ApiError::Internal(e.into()))?;
    let profile = profiles::update(state.store.pool(), id, body.name.trim(), &config)
        .await
        .map_err(name_conflict)?
        .ok_or_else(|| ApiError::NotFound(format!("load profile {id}")))?;

    tracing::info!(actor = %actor.username, "load profile updated");
    Ok(Json(profile))
}

/// Deletes a profile, unless a test depends on it.
#[tracing::instrument(skip(state), fields(profile_id = %id))]
async fn delete(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<serde_json::Value>> {
    let dependents = profiles::referencing_tests(state.store.pool(), id).await?;
    if !dependents.is_empty() {
        return Err(ApiError::Conflict(format!(
            "this profile is used by {}; remove it from them first",
            dependents.join(", ")
        )));
    }

    if !profiles::delete(state.store.pool(), id).await? {
        return Err(ApiError::NotFound(format!("load profile {id}")));
    }

    tracing::info!(actor = %actor.username, "load profile deleted");
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// What a profile would actually generate.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePreview {
    /// Distinct client address/port pairs available.
    pub client_capacity: u64,
    /// Distinct server addresses available.
    pub server_addresses: u64,
    /// Bytes one completed conversation moves.
    pub bytes_per_connection: u64,
    /// Bits per second implied once the rate reaches its target.
    pub implied_bps: f64,
    /// The transmitting port's line speed, or zero when it has none.
    pub client_port_speed_mbps: u32,
    /// True when the implied throughput exceeds the client port.
    pub exceeds_line_rate: bool,
    /// Seconds before results should be considered meaningful.
    pub measurement_starts_at: f64,
    /// One-line summary for the editor.
    pub summary: String,
}

/// Reports what a profile implies, without saving it.
///
/// The throughput figure is the point: a connection rate and a response size
/// that each look reasonable multiply into something that may be well past the
/// link, and that is far easier to see here than from a run that quietly tops
/// out at the port's capacity.
async fn preview(
    State(state): State<AppState>,
    _auth: Auth,
    Json(body): Json<ProfileInput>,
) -> ApiResult<Json<ProfilePreview>> {
    body.validate()?;

    // A preview may name a port that has just been removed, so a missing port
    // yields no line rate rather than an error.
    let speed = match ports::get(state.store.pool(), body.config.client_port).await? {
        Some(port) => port.speed_mbps.unwrap_or(0).max(0) as u32,
        None => 0,
    };

    let implied = translate::implied_bits_per_second(&body.config);
    let port_bps = f64::from(speed) * 1_000_000.0;

    Ok(Json(ProfilePreview {
        client_capacity: body.config.client_pool.capacity(),
        server_addresses: body.config.server_pool.address_count(),
        bytes_per_connection: body.config.app.bytes_per_connection(),
        implied_bps: implied,
        client_port_speed_mbps: speed,
        exceeds_line_rate: port_bps > 0.0 && implied > port_bps,
        measurement_starts_at: body.config.ramp.measurement_starts_at(),
        summary: summarise(&body.config, implied),
    }))
}

/// The one-line description shown under the editor.
fn summarise(config: &LoadProfileConfig, implied_bps: f64) -> String {
    format!(
        "{} conn/s, up to {} concurrent, {} per connection = {}",
        format_count(config.target_cps as u64),
        format_count(config.max_concurrent),
        format_bytes(config.app.bytes_per_connection()),
        format_bitrate(implied_bps),
    )
}

/// Renders a count with thousands separators.
fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Renders a byte count with binary prefixes.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Renders a bit rate with decimal prefixes.
fn format_bitrate(bps: f64) -> String {
    if !bps.is_finite() || bps <= 0.0 {
        return "unknown".into();
    }
    if bps >= 1e9 {
        format!("{:.2} Gb/s", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.2} Mb/s", bps / 1e6)
    } else {
        format!("{:.0} b/s", bps)
    }
}

/// Rejects a profile naming a port that does not exist.
async fn check_ports(state: &AppState, config: &LoadProfileConfig) -> ApiResult<()> {
    let mut errors = Vec::new();

    for (field, port_id) in
        [("clientPort", config.client_port), ("serverPort", config.server_port)]
    {
        if ports::get(state.store.pool(), port_id).await?.is_none() {
            errors.push(flux_core::config::FieldError::new(
                format!("config.{field}"),
                "no port with that id",
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// Turns a duplicate-name insert into a field-level conflict.
fn name_conflict(err: sqlx::Error) -> ApiError {
    if is_unique_violation(&err) {
        ApiError::field("name", "a load profile with that name already exists")
    } else {
        err.into()
    }
}

#[cfg(test)]
mod tests {
    use flux_core::profile::{AppSpec, IpPool, Ramp};

    use super::*;

    /// A profile to summarise.
    fn config() -> LoadProfileConfig {
        LoadProfileConfig {
            client_port: Id::from_u128(1),
            server_port: Id::from_u128(2),
            client_pool: IpPool { cidr: "16.0.0.0/16".into(), port_min: 1024, port_max: 65535 },
            server_pool: IpPool { cidr: "48.0.0.0/24".into(), port_min: 80, port_max: 80 },
            app: AppSpec::Raw { request_bytes: 200, response_bytes: 32_568 },
            target_cps: 10_000.0,
            max_concurrent: 100_000,
            ramp: Ramp { warmup_secs: 10.0, settle_secs: 5.0 },
            duration_secs: None,
        }
    }

    #[test]
    fn the_summary_names_the_rate_concurrency_and_implied_throughput() {
        let c = config();
        let summary = summarise(&c, translate::implied_bits_per_second(&c));

        assert!(summary.contains("10,000 conn/s"), "{summary}");
        assert!(summary.contains("100,000 concurrent"), "{summary}");
        assert!(summary.contains("32.0 KiB"), "{summary}");
        assert!(summary.contains("Gb/s"), "{summary}");
    }

    #[test]
    fn a_profile_whose_conversation_size_is_unknown_says_so() {
        // A capture's size is not known until it is loaded; reporting zero
        // throughput would look like a measurement rather than a gap.
        let mut c = config();
        c.app = AppSpec::Pcap { pcap_ref: "web.pcap".into() };

        let summary = summarise(&c, translate::implied_bits_per_second(&c));
        assert!(summary.contains("unknown"), "{summary}");
    }

    #[test]
    fn byte_counts_pick_a_readable_unit() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(32_768), "32.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn bit_rates_pick_a_readable_unit() {
        assert_eq!(format_bitrate(2_600_000_000.0), "2.60 Gb/s");
        assert_eq!(format_bitrate(1_500_000.0), "1.50 Mb/s");
        assert_eq!(format_bitrate(0.0), "unknown");
    }

    #[test]
    fn counts_are_grouped_for_reading() {
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000_000), "1,000,000");
    }
}
