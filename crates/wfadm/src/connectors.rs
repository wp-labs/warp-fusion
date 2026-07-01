//! Connector template generation — mirrors wp-proj's connectors pattern.
//!
//! Registers built-in source/sink factories into `wp_core_connectors::registry`,
//! then generates template files in `connectors/source.d/` and `connectors/sink.d/`
//! from the registered connector definitions.
//!
//! This ensures connector templates always match the actual connector registry,
//! regardless of factory additions or removals in `wp-core-connectors`.

use std::fs;
use std::path::Path;
use std::sync::Once;

use toml::Value;
use wp_connector_api::ConnectorDef;
use wp_core_connectors::registry;

// ── Factory registration ──────────────────────────────────────────────

static FACTORIES_REGISTERED: Once = Once::new();

/// Register all built-in connector factories.
///
/// Safe to call multiple times — registration happens exactly once.
pub fn ensure_factories_registered() {
    FACTORIES_REGISTERED.call_once(|| {
        // ── Sources ──
        wp_core_connectors::sources::file::register_factory_only();
        wp_core_connectors::sources::tcp::factory::register_tcp_factory();
        wp_core_connectors::sources::syslog::register_syslog_factory();

        // ── Sinks ──
        registry::register_sink_factory(
            wp_core_connectors::sinks::blackhole_factory::BlackHoleFactory,
        );
        registry::register_sink_factory(wp_core_connectors::sinks::file_factory::FileFactory);
        registry::register_sink_factory(wp_core_connectors::sinks::syslog::SyslogFactory);
        registry::register_sink_factory(wp_core_connectors::sinks::tcp::TcpFactory);
    });
}

// ── Template generation ───────────────────────────────────────────────

/// Generate connector template files from the registry.
///
/// Writes to `connectors/source.d/` and `connectors/sink.d/` under `work_root`.
/// Existing files are **not** overwritten.
pub fn generate_connector_templates(work_root: &Path) -> Result<(), String> {
    ensure_factories_registered();

    let source_dir = work_root.join("connectors/source.d");
    let sink_dir = work_root.join("connectors/sink.d");

    let mut source_defs = registry::registered_source_defs();
    let mut sink_defs = registry::registered_sink_defs();

    // Stable ordering: sort by connector kind so generated file names
    // and content are deterministic across runs.
    source_defs.sort_by(|a, b| a.id.cmp(&b.id));
    sink_defs.sort_by(|a, b| a.id.cmp(&b.id));

    if !source_defs.is_empty() {
        fs::create_dir_all(&source_dir).map_err(|e| format!("create connectors/source.d: {e}"))?;
        for (idx, def) in source_defs.iter().enumerate() {
            let file_name = format!("{:02}-{}.toml", idx, slugify(&def.id));
            write_if_absent(&source_dir.join(&file_name), std::slice::from_ref(def))?;
        }
    }

    if !sink_defs.is_empty() {
        fs::create_dir_all(&sink_dir).map_err(|e| format!("create connectors/sink.d: {e}"))?;
        for (idx, def) in sink_defs.iter().enumerate() {
            let file_name = format!("{:02}-{}.toml", idx, slugify(&def.id));
            write_if_absent(&sink_dir.join(&file_name), std::slice::from_ref(def))?;
        }
    }

    Ok(())
}

fn write_if_absent(path: &Path, defs: &[ConnectorDef]) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let body = render_connector_file(defs)?;
    fs::write(path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

// ── Rendering ─────────────────────────────────────────────────────────

fn render_connector_file(defs: &[ConnectorDef]) -> Result<String, String> {
    let entries: Vec<Value> = defs.iter().map(connector_to_value).collect();
    let mut root = toml::value::Table::new();
    root.insert("connectors".to_string(), Value::Array(entries));
    toml::to_string(&Value::Table(root)).map_err(|e| format!("serialize connector TOML: {e}"))
}

fn connector_to_value(def: &ConnectorDef) -> Value {
    let mut entry = toml::value::Table::new();
    entry.insert("id".into(), Value::String(def.id.clone()));
    entry.insert("type".into(), Value::String(def.kind.clone()));

    if !def.allow_override.is_empty() {
        let arr: Vec<Value> = def
            .allow_override
            .iter()
            .map(|s| Value::String(s.clone()))
            .collect();
        entry.insert("allow_override".into(), Value::Array(arr));
    }

    if !def.default_params.is_empty() {
        entry.insert("params".into(), param_map_to_toml(&def.default_params));
    }

    Value::Table(entry)
}

fn param_map_to_toml(params: &wp_connector_api::ParamMap) -> Value {
    let mut table = toml::value::Table::new();
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();
    for key in keys {
        table.insert(key.clone(), json_to_toml(&params[key]));
    }
    Value::Table(table)
}

fn json_to_toml(val: &serde_json::Value) -> Value {
    match val {
        serde_json::Value::Null => Value::String("".to_string()),
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => Value::Array(arr.iter().map(json_to_toml).collect()),
        serde_json::Value::Object(obj) => {
            let mut table = toml::value::Table::new();
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for k in keys {
                table.insert(k.clone(), json_to_toml(&obj[k]));
            }
            Value::Table(table)
        }
    }
}

fn slugify(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wfadm_conn_{}_{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn registration_does_not_panic() {
        ensure_factories_registered();
        // Calling twice is safe
        ensure_factories_registered();
    }

    #[test]
    fn registered_sinks_non_empty() {
        ensure_factories_registered();
        let sinks = registry::registered_sink_defs();
        assert!(!sinks.is_empty(), "should have registered sink factories");
        let kinds = registry::list_sink_kinds();
        assert!(kinds.iter().any(|k| k == "file"));
    }

    #[test]
    fn registered_sources_non_empty() {
        ensure_factories_registered();
        let sources = registry::registered_source_defs();
        assert!(
            !sources.is_empty(),
            "should have registered source factories"
        );
        let kinds = registry::list_source_kinds();
        assert!(kinds.iter().any(|k| k == "file"));
    }

    #[test]
    fn generates_connector_templates() {
        ensure_factories_registered();
        let dir = temp_dir();
        generate_connector_templates(&dir).expect("generate connectors");

        let sink_d = dir.join("connectors/sink.d");
        assert!(sink_d.is_dir(), "sink.d should exist");
        let sink_files: Vec<_> = std::fs::read_dir(&sink_d)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(!sink_files.is_empty(), "sink.d should have files");

        let source_d = dir.join("connectors/source.d");
        assert!(source_d.is_dir(), "source.d should exist");
        let source_files: Vec<_> = std::fs::read_dir(&source_d)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(!source_files.is_empty(), "source.d should have files");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_does_not_overwrite_existing() {
        ensure_factories_registered();
        let dir = temp_dir();
        generate_connector_templates(&dir).expect("first gen");

        // Modify a generated file
        let first_sink = std::fs::read_dir(dir.join("connectors/sink.d"))
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().map(|x| x == "toml").unwrap_or(false))
            .expect("at least one .toml file in sink.d")
            .path();
        let _original = std::fs::read_to_string(&first_sink).unwrap();
        std::fs::write(&first_sink, "modified").unwrap();

        // Second generation should NOT overwrite
        generate_connector_templates(&dir).expect("second gen");
        assert_eq!(std::fs::read_to_string(&first_sink).unwrap(), "modified");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slugify_replaces_special_chars() {
        let result = slugify("my connector!");
        assert_eq!(result, "my_connector_");
    }
}
