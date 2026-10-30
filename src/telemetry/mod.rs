/// Telemetry collection and reporting.
///
/// The sync pipeline calls [`reporter::report_metrics`] on every successful
/// sync to submit pseudonymous usage counters (fire-and-forget). Telemetry can
/// be disabled at runtime via the `ACTUAL_NO_TELEMETRY` env var or the
/// `telemetry.enabled` config flag, or at compile time by disabling the
/// `telemetry` Cargo feature.
pub mod identity;
pub mod metrics;
pub mod opt_out;
#[cfg(feature = "telemetry")]
pub mod otlp;
pub mod plan_governance;
pub mod reporter;
pub mod scope;
#[cfg(feature = "telemetry")]
pub mod sign_in;
#[cfg(feature = "telemetry")]
pub mod spool;
