//! Flow endpoints: CRUD, plus the frame preview the editor renders.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Router;
use flux_core::config::Validate;
use flux_core::flow::{FlowConfig, FrameSize};
use flux_core::frame;
use flux_core::rate::{self, ResolvedRate};
use flux_core::types::Id;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, ApiResult};
use super::extract::{Auth, Json, OperatorAuth};
use crate::state::AppState;
use crate::store::models::Flow;
use crate::store::{flows, is_unique_violation, ports};

/// Mounts the flow routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/preview", post(preview))
        .route("/{id}", get(get_one).put(update).delete(delete))
}

/// Every flow.
async fn list(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<Flow>>> {
    Ok(Json(flows::list(state.store.pool()).await?))
}

/// One flow.
async fn get_one(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<Id>,
) -> ApiResult<Json<Flow>> {
    flows::get(state.store.pool(), id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("flow {id}")))
}

/// The body for creating or replacing a flow.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlowInput {
    /// Operator-assigned label.
    pub name: String,
    /// The flow definition.
    pub config: FlowConfig,
}

impl FlowInput {
    /// Checks the name and delegates the document to its own validation.
    fn validate(&self) -> ApiResult<()> {
        let mut v = flux_core::config::Validation::new();

        let name = self.name.trim();
        v.require(!name.is_empty(), "name", "must not be empty");
        v.require(name.chars().count() <= 64, "name", "must be at most 64 characters");

        v.scope("config", |v| self.config.validate_into(v));
        v.finish()?;
        Ok(())
    }
}

/// Creates a flow.
#[tracing::instrument(skip_all, fields(name = %body.name))]
async fn create(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Json(body): Json<FlowInput>,
) -> ApiResult<Json<Flow>> {
    body.validate()?;
    check_ports_exist(&state, &body.config).await?;

    let config = serde_json::to_value(&body.config).map_err(|e| ApiError::Internal(e.into()))?;
    let flow = flows::create(state.store.pool(), body.name.trim(), &config, Some(actor.user_id))
        .await
        .map_err(name_conflict)?;

    tracing::info!(actor = %actor.username, flow_id = %flow.id, "flow created");
    Ok(Json(flow))
}

/// Replaces a flow.
#[tracing::instrument(skip_all, fields(flow_id = %id))]
async fn update(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
    Json(body): Json<FlowInput>,
) -> ApiResult<Json<Flow>> {
    body.validate()?;
    check_ports_exist(&state, &body.config).await?;

    let config = serde_json::to_value(&body.config).map_err(|e| ApiError::Internal(e.into()))?;
    let flow = flows::update(state.store.pool(), id, body.name.trim(), &config)
        .await
        .map_err(name_conflict)?
        .ok_or_else(|| ApiError::NotFound(format!("flow {id}")))?;

    tracing::info!(actor = %actor.username, "flow updated");
    Ok(Json(flow))
}

/// Deletes a flow, unless a test depends on it.
#[tracing::instrument(skip(state), fields(flow_id = %id))]
async fn delete(
    State(state): State<AppState>,
    OperatorAuth(actor): OperatorAuth,
    Path(id): Path<Id>,
) -> ApiResult<Json<serde_json::Value>> {
    // Deleting a flow a test depends on would leave that test unable to run with
    // nothing to say why, so the refusal names what is in the way.
    let dependents = flows::referencing_tests(state.store.pool(), id).await?;
    if !dependents.is_empty() {
        return Err(ApiError::Conflict(format!(
            "this flow is used by {}; remove it from them first",
            dependents.join(", ")
        )));
    }

    if !flows::delete(state.store.pool(), id).await? {
        return Err(ApiError::NotFound(format!("flow {id}")));
    }

    tracing::info!(actor = %actor.username, "flow deleted");
    Ok(Json(serde_json::json!({ "deleted": true })))
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// What a flow would actually put on the wire.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlowPreview {
    /// One entry per distinct frame length the flow emits.
    pub frames: Vec<FramePreview>,
    /// Total header length in bytes.
    pub header_bytes: u32,
    /// The rate this flow resolves to on its transmitting port.
    pub rate: ResolvedRate,
    /// Transmitting port's line speed, or zero when it has none.
    pub port_speed_mbps: u32,
    /// True when the requested rate exceeds the port.
    pub exceeds_line_rate: bool,
    /// How many distinct values the modifiers produce in combination.
    pub variant_count: u64,
    /// One-line summary, as shown in the editor's status bar.
    pub summary: String,
}

/// One generated frame.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FramePreview {
    /// On-wire length including FCS.
    pub wire_len: u32,
    /// The bytes the NIC transmits, excluding the FCS it appends.
    pub bytes: Vec<u8>,
    /// A classic hex dump of those bytes.
    pub hex_dump: String,
}

/// Renders what a flow would generate, without saving it.
///
/// The frame is built in Rust rather than in the browser because the derived
/// fields — EtherTypes, lengths, and three checksums — are exactly the kind of
/// thing that goes subtly wrong in a second implementation. A preview that
/// disagrees with what the engine transmits is worse than no preview.
async fn preview(
    State(state): State<AppState>,
    _auth: Auth,
    Json(body): Json<FlowInput>,
) -> ApiResult<Json<FlowPreview>> {
    body.validate()?;

    // A preview of an unsaved flow may legitimately name a port that has just
    // been removed, so a missing port yields no line rate rather than an error.
    let speed = match ports::get(state.store.pool(), body.config.tx_port).await? {
        Some(port) => port.speed_mbps.unwrap_or(0).max(0) as u32,
        None => 0,
    };

    let mut frames = Vec::new();
    for wire_len in frame::sizes_in(&body.config.size) {
        let built = frame::build_with_size(&body.config, wire_len)
            .map_err(|e| ApiError::field("config.size", e.to_string()))?;
        frames.push(FramePreview {
            wire_len: built.wire_len,
            hex_dump: built.hex_dump(),
            bytes: built.bytes,
        });
    }

    let resolved = rate::resolve_for_size(&body.config.rate, &body.config.size, speed);
    let variants = variant_count(&body.config);

    Ok(Json(FlowPreview {
        header_bytes: body.config.header_bytes(),
        summary: summarise(&body.config, &resolved, variants),
        rate: resolved,
        port_speed_mbps: speed,
        exceeds_line_rate: resolved.exceeds_line_rate(),
        variant_count: variants,
        frames,
    }))
}

/// How many distinct frames the modifiers produce in combination.
///
/// Saturating: three modifiers of a thousand values each is a billion, and the
/// number is for display rather than for arithmetic.
fn variant_count(config: &FlowConfig) -> u64 {
    config
        .modifiers
        .iter()
        .map(|m| u64::from(m.count))
        .fold(1u64, |acc, n| acc.saturating_mul(n.max(1)))
}

/// The one-line description shown under the editor.
fn summarise(config: &FlowConfig, resolved: &ResolvedRate, variants: u64) -> String {
    let size = match &config.size {
        FrameSize::Fixed { bytes } => format!("{bytes}B"),
        FrameSize::Imix { preset } => {
            format!("IMIX avg {:.0}B", preset.average_bytes())
        }
        FrameSize::Random { min, max } => format!("{min}-{max}B"),
    };

    let variants = if variants > 1 {
        format!("{} variants, ", format_count(variants))
    } else {
        String::new()
    };

    format!(
        "{variants}{size}, {} = {}",
        format_pps(resolved.pps),
        format_bps(resolved.bps_l1)
    )
}

/// Renders a packet rate with a unit.
fn format_pps(pps: f64) -> String {
    if pps >= 1_000_000.0 {
        format!("{:.2} Mpps", pps / 1_000_000.0)
    } else if pps >= 1_000.0 {
        format!("{:.2} kpps", pps / 1_000.0)
    } else {
        format!("{pps:.0} pps")
    }
}

/// Renders a bit rate with a unit.
fn format_bps(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{:.2} Gbps", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.2} Mbps", bps / 1e6)
    } else {
        format!("{:.0} bps", bps)
    }
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

/// Rejects a flow naming a port that does not exist.
async fn check_ports_exist(state: &AppState, config: &FlowConfig) -> ApiResult<()> {
    let mut errors = Vec::new();

    for (field, port_id) in [("txPort", config.tx_port), ("rxPort", config.rx_port)] {
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
        ApiError::field("name", "a flow with that name already exists")
    } else {
        err.into()
    }
}

#[cfg(test)]
mod tests {
    use flux_core::flow::{
        EthernetFields, HeaderLayer, ImixPreset, Ipv4Fields, Modifier, ModifierField,
        ModifierMode, Rate, UdpFields,
    };

    use super::*;

    /// A flow at a fixed size.
    fn config() -> FlowConfig {
        FlowConfig {
            tx_port: Id::nil(),
            rx_port: Id::nil(),
            headers: vec![
                HeaderLayer::Ethernet(EthernetFields::default()),
                HeaderLayer::Ipv4(Ipv4Fields::default()),
                HeaderLayer::Udp(UdpFields::default()),
            ],
            size: FrameSize::Fixed { bytes: 512 },
            rate: Rate::Percent { value: 100.0 },
            modifiers: Vec::new(),
            duration_secs: None,
            latency_track: false,
        }
    }

    #[test]
    fn a_flow_with_no_modifiers_has_a_single_variant() {
        assert_eq!(variant_count(&config()), 1);
    }

    #[test]
    fn modifier_counts_multiply_into_the_variant_total() {
        let mut c = config();
        c.modifiers = vec![
            Modifier {
                field: ModifierField::Ipv4Src,
                mode: ModifierMode::Increment,
                count: 1000,
                step: 1,
            },
            Modifier {
                field: ModifierField::L4DstPort,
                mode: ModifierMode::Increment,
                count: 10,
                step: 1,
            },
        ];
        assert_eq!(variant_count(&c), 10_000);
    }

    #[test]
    fn an_enormous_variant_count_saturates_rather_than_wrapping() {
        let mut c = config();
        c.modifiers = (0..8)
            .map(|_| Modifier {
                field: ModifierField::Ipv4Src,
                mode: ModifierMode::Increment,
                count: u32::MAX,
                step: 1,
            })
            .collect();
        assert_eq!(variant_count(&c), u64::MAX);
    }

    #[test]
    fn the_summary_names_the_size_rate_and_throughput() {
        let c = config();
        let resolved = rate::resolve_for_size(&c.rate, &c.size, 10_000);
        let summary = summarise(&c, &resolved, 1);

        assert!(summary.contains("512B"), "{summary}");
        assert!(summary.contains("Mpps"), "{summary}");
        assert!(summary.contains("10.00 Gbps"), "{summary}");
    }

    #[test]
    fn the_summary_mentions_variants_only_when_there_are_several() {
        let c = config();
        let resolved = rate::resolve_for_size(&c.rate, &c.size, 10_000);

        assert!(!summarise(&c, &resolved, 1).contains("variant"));
        assert!(summarise(&c, &resolved, 10_000).contains("10,000 variants"));
    }

    #[test]
    fn a_mixture_summary_reports_its_average_size() {
        let mut c = config();
        c.size = FrameSize::Imix { preset: ImixPreset::Simple };
        let resolved = rate::resolve_for_size(&c.rate, &c.size, 10_000);

        let summary = summarise(&c, &resolved, 1);
        assert!(summary.contains("IMIX avg 354B"), "{summary}");
    }

    #[test]
    fn counts_are_grouped_for_reading() {
        assert_eq!(format_count(1), "1");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(10_000), "10,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn rates_pick_a_readable_unit() {
        assert_eq!(format_pps(500.0), "500 pps");
        assert_eq!(format_pps(1_500.0), "1.50 kpps");
        assert_eq!(format_pps(14_880_952.0), "14.88 Mpps");

        assert_eq!(format_bps(10e9), "10.00 Gbps");
        assert_eq!(format_bps(1.5e6), "1.50 Mbps");
    }
}
