use wf_lang::{FieldType, WindowSchema};

use super::ValidationError;
use super::gen_compat::check_gen_expr_compat;
use crate::wfg_ast::ScenarioDecl;

/// SC3, SC4, SV7: stream-schema cross-checks (window exists, field names, type compat).
pub(super) fn validate_streams_with_schemas(
    scenario: &ScenarioDecl,
    schemas: &[WindowSchema],
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // SC3: stream.window must exist in schemas
    for stream in &scenario.streams {
        if !schemas.iter().any(|s| s.name == stream.window) {
            errors.push(ValidationError {
                code: "SC3",
                message: format!(
                    "stream '{}': window '{}' not found in schemas",
                    stream.alias, stream.window
                ),
            });
        }
    }

    // SC4: field_override field names must exist in the window schema
    for stream in &scenario.streams {
        if let Some(schema) = schemas.iter().find(|s| s.name == stream.window) {
            for ov in &stream.overrides {
                if !schema.fields.iter().any(|f| f.name == ov.field_name) {
                    errors.push(ValidationError {
                        code: "SC4",
                        message: format!(
                            "stream '{}': field '{}' not found in window '{}'",
                            stream.alias, ov.field_name, stream.window
                        ),
                    });
                }
            }
        }
    }

    // SV7: gen_expr type compatibility with field type
    for stream in &scenario.streams {
        if let Some(schema) = schemas.iter().find(|s| s.name == stream.window) {
            for ov in &stream.overrides {
                if let Some(field_def) = schema.fields.iter().find(|f| f.name == ov.field_name) {
                    let base = match &field_def.field_type {
                        FieldType::Base(b) => b,
                        FieldType::Array(b) => b,
                    };
                    if let Some(reason) = check_gen_expr_compat(&ov.gen_expr, base) {
                        errors.push(ValidationError {
                            code: "SV7",
                            message: format!(
                                "stream '{}': field '{}' ({:?}) incompatible with override — {}",
                                stream.alias, ov.field_name, base, reason
                            ),
                        });
                    }
                }
            }
        }
    }

    errors
}
