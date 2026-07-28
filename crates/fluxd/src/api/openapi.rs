//! OpenAPI description of the REST surface.
//!
//! Generated from the same Rust types the handlers use, so the document cannot
//! describe a field the API does not actually return. It is served rather than
//! written to a file so it always matches the running binary — the appliance may
//! be several versions ahead of or behind any checked-in copy.

use axum::Json;
use utoipa::OpenApi;

use super::error::ErrorBody;
use super::{auth, port_groups, ports, system, users};

/// The generated API description.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Flux",
        description = "Traffic generation and load testing that moves at line rate.",
        license(name = "MIT"),
    ),
    components(schemas(
        ErrorBody,
        flux_core::config::FieldError,
        flux_core::types::Role,
        flux_core::types::PortMode,
        flux_core::types::LinkState,
        flux_core::types::EngineMode,
        flux_core::types::PortGroupState,
        flux_core::port::HugepageSize,
        flux_core::port::HugepagePool,
        flux_core::port::HugepagesStatus,
        auth::LoginRequest,
        auth::MeResponse,
        ports::PortUpdate,
        ports::BulkPortUpdate,
        ports::BulkPortUpdateEntry,
        ports::ReserveRequest,
        ports::ReleaseResponse,
        port_groups::PortGroupInput,
        port_groups::PortGroupView,
        users::CreateUser,
        users::UpdateUser,
        system::Health,
        system::SubsystemHealth,
        system::PortCounts,
        system::DiskUsage,
        system::HugepagesRequest,
        crate::store::models::UserView,
        crate::store::models::PortView,
        crate::store::models::PortGroup,
        crate::store::models::PortGroupRef,
        crate::store::models::ReservationView,
        crate::store::models::Setting,
    )),
    tags(
        (name = "auth", description = "Sessions and identity"),
        (name = "ports", description = "Port inventory, binding, and reservations"),
        (name = "port-groups", description = "Engine instance groupings"),
        (name = "users", description = "Account administration"),
        (name = "system", description = "Health, hugepages, and settings"),
    )
)]
pub struct ApiDoc;

/// Serves the OpenAPI document.
pub async fn document() -> Json<utoipa::openapi::OpenApi> {
    let mut doc = ApiDoc::openapi();
    doc.info.version = super::system::VERSION.to_string();
    Json(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_document_builds_and_names_the_product() {
        // The derive can fail at runtime if a referenced schema is missing, so
        // building it in a test is what keeps the endpoint from 500-ing in the
        // field.
        let doc = ApiDoc::openapi();
        assert_eq!(doc.info.title, "Flux");
        assert!(doc.components.is_some(), "component schemas should be registered");
    }
}
