// NOTE: The telemetry module (reporter::report_metrics) is not currently
// called from the sync pipeline. It is preserved for future integration.
// See actual-cli-bek.13.
pub mod identity;
pub mod metrics;
pub mod reporter;
