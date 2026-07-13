//! App-level registration of external connector factories.
//!
//! Mirrors the pattern in `warp-parse/src/feats.rs`: centralized registration
//! functions that can be reused across targets, keeping feature-coupling out of
//! the core library.
//!
//! All registration functions are safe to call multiple times — the global
//! registry ignores duplicate entries.

/// Register all connector factories needed at runtime.
///
/// Called once during engine startup before bootstrapping sinks and sources.
pub fn register_connectors() {
    use wp_core_connectors::registry::{register_sink_factory, register_source_factory};
    
    // syslog
    wp_core_connectors::sources::syslog::register_syslog_factory();

    // syslog
    wp_core_connectors::sources::syslog::register_syslog_factory();

    // Kafka
    register_source_factory(wp_connectors::kafka::KafkaSourceFactory);
    register_sink_factory(wp_connectors::kafka::KafkaSinkFactory);

    // Doris
    register_sink_factory(wp_connectors::doris::DorisSinkFactory);

    // VictoriaMetrics
    register_sink_factory(wp_connectors::victoriametrics::VictoriaMetricFactory);
}
