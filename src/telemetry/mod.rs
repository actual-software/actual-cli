/// Telemetry collection and reporting.
///
/// The sync pipeline calls [`reporter::report_metrics`] on every successful
/// sync to submit anonymous usage counters (fire-and-forget). Telemetry can
/// be disabled at runtime via the `ACTUAL_NO_TELEMETRY` env var or the
/// `telemetry.enabled` config flag, or at compile time by disabling the
/// `telemetry` Cargo feature.
pub mod identity;
pub mod metrics;
pub mod reporter;
