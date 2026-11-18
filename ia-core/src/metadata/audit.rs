use serde::Serialize;
use std::collections::HashMap;

use crate::metadata::schema::SchemaField;
use crate::types::{MetadataFields, MetadataValue};

/// Severity level for an audit finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// What kind of issue was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// Field value is `MetadataValue::Other` (unexpected JSON type).
    TypeMismatch,
    /// Non-repeatable field has multiple values.
    Repeatability,
    /// Required or recommended field is absent.
    MissingRequired,
    /// Schema says the field is deprecated but it is present.
    Deprecated,
    /// Field exists in metadata but not in the schema.
    Unknown,
}

/// A single finding from auditing one metadata field.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// The metadata field name (e.g., "date", "collection").
    pub field: String,
    /// Severity of this finding.
    pub severity: Severity,
    /// Structured kind for programmatic filtering.
    pub kind: FindingKind,
    /// Human-readable description of the issue.
    pub message: String,
}

/// Result of auditing one item's metadata against the schema.
#[derive(Debug, Clone, Serialize)]
pub struct AuditResult {
    /// The item identifier.
    pub identifier: String,
    /// All findings for this item.
    pub findings: Vec<Finding>,
}

impl AuditResult {
    /// True if there are no findings at all.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Count findings of a given severity.
    pub fn count(&self, severity: &Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| &f.severity == severity)
            .count()
    }
}

/// Audit an item's metadata fields against the IA metadata schema.
///
/// Checks for:
/// 1. Type mismatches — field value is `MetadataValue::Other` (unexpected type)
/// 2. Repeatability violations — non-repeatable field has multiple values
/// 3. Missing required fields — schema says required but field is absent
/// 4. Deprecated fields — schema says deprecated but field is present
/// 5. Unknown fields — field exists but not in schema (informational)
pub fn audit_item(
    identifier: &str,
    metadata: &MetadataFields,
    schema: &[SchemaField],
) -> AuditResult {
    let mut findings = Vec::new();
    let schema_map: HashMap<&str, &SchemaField> =
        schema.iter().map(|f| (f.field.as_str(), f)).collect();

    // Extra fields arrive as `serde_json::Value`. Convert each one with the
    // same deserialization the typed fields use, so every field gets the
    // same checks. A `null` is left out, as a typed field's `null`
    // deserializes to `None`.
    let extra_values: Vec<(&str, MetadataValue)> = metadata
        .extra
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| (key.as_str(), to_metadata_value(value)))
        .collect();

    // Every present field, typed and extra, by name.
    let mut all_fields: HashMap<&str, &MetadataValue> = HashMap::new();
    add_typed_field(&mut all_fields, "identifier", &metadata.identifier);
    add_typed_field(&mut all_fields, "title", &metadata.title);
    add_typed_field(&mut all_fields, "description", &metadata.description);
    add_typed_field(&mut all_fields, "mediatype", &metadata.mediatype);
    add_typed_field(&mut all_fields, "collection", &metadata.collection);
    add_typed_field(&mut all_fields, "creator", &metadata.creator);
    add_typed_field(&mut all_fields, "date", &metadata.date);
    add_typed_field(&mut all_fields, "subject", &metadata.subject);
    add_typed_field(&mut all_fields, "language", &metadata.language);
    add_typed_field(&mut all_fields, "publicdate", &metadata.publicdate);
    add_typed_field(&mut all_fields, "addeddate", &metadata.addeddate);
    add_typed_field(&mut all_fields, "uploader", &metadata.uploader);
    for (key, value) in &extra_values {
        all_fields.insert(key, value);
    }

    for (&field_name, &value) in &all_fields {
        let Some(schema_field) = schema_map.get(field_name) else {
            // Field not in schema — informational.
            findings.push(Finding {
                field: field_name.to_string(),
                severity: Severity::Info,
                kind: FindingKind::Unknown,
                message: "field not in schema".to_string(),
            });
            continue;
        };

        // Check for unexpected type.
        if value.is_other() {
            findings.push(Finding {
                field: field_name.to_string(),
                severity: Severity::Warning,
                kind: FindingKind::TypeMismatch,
                message: format!("unexpected type: got {}", describe_value(&value.as_value())),
            });
        }

        // Check repeatability: non-repeatable but has multiple values.
        if schema_field.repeatable == "No" {
            if let MetadataValue::Multiple(v) = value {
                if v.len() > 1 {
                    findings.push(Finding {
                        field: field_name.to_string(),
                        severity: Severity::Warning,
                        kind: FindingKind::Repeatability,
                        message: format!("not repeatable but has {} values", v.len()),
                    });
                }
            }
        }

        // Check deprecated.
        if schema_field.required == "Deprecated" {
            findings.push(Finding {
                field: field_name.to_string(),
                severity: Severity::Info,
                kind: FindingKind::Deprecated,
                message: "deprecated field is present".to_string(),
            });
        }
    }

    // Check for missing required fields.
    for schema_field in schema {
        let required = schema_field.required == "Yes" || schema_field.required == "Recommended";
        if required && !all_fields.contains_key(schema_field.field.as_str()) {
            let severity = if schema_field.required == "Yes" {
                Severity::Error
            } else {
                Severity::Warning
            };
            findings.push(Finding {
                field: schema_field.field.clone(),
                severity,
                kind: FindingKind::MissingRequired,
                message: format!("missing {} field", schema_field.required.to_lowercase()),
            });
        }
    }

    // Sort findings by severity (Error first, then Warning, then Info),
    // then alphabetically by field name.
    findings.sort_by(|a, b| {
        let sev_ord = |s: &Severity| match s {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
        };
        sev_ord(&a.severity)
            .cmp(&sev_ord(&b.severity))
            .then(a.field.cmp(&b.field))
    });

    AuditResult {
        identifier: identifier.to_string(),
        findings,
    }
}

fn add_typed_field<'a>(
    map: &mut HashMap<&'a str, &'a MetadataValue>,
    name: &'a str,
    value: &'a Option<MetadataValue>,
) {
    if let Some(v) = value {
        map.insert(name, v);
    }
}

/// Convert an untyped metadata value to a [`MetadataValue`], the way a typed
/// field is deserialized: a string, an array of strings, or anything else
/// as `Other`.
fn to_metadata_value(value: &serde_json::Value) -> MetadataValue {
    serde_json::from_value(value.clone()).unwrap_or_else(|_| MetadataValue::Other(value.clone()))
}

/// Describe a JSON value type for human-readable messages.
fn describe_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => format!("boolean ({b})"),
        serde_json::Value::Number(n) => format!("number ({n})"),
        serde_json::Value::String(s) => format!("string ({s:?})"),
        serde_json::Value::Array(a) => format!(
            "array of {} {}",
            a.len(),
            if a.len() == 1 { "element" } else { "elements" }
        ),
        serde_json::Value::Object(o) => format!(
            "object with {} {}",
            o.len(),
            if o.len() == 1 { "key" } else { "keys" }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MetadataFields;

    fn test_schema() -> Vec<SchemaField> {
        vec![
            SchemaField {
                field: "identifier".to_string(),
                label: "Identifier".to_string(),
                required: "Yes".to_string(),
                repeatable: "No".to_string(),
                internal_use_only: "No".to_string(),
                defined_by: "uploader".to_string(),
                edit_access: "not editable".to_string(),
                definition: String::new(),
                accepted_values: "String".to_string(),
                usage_notes: String::new(),
                example: vec![],
            },
            SchemaField {
                field: "title".to_string(),
                label: "Title".to_string(),
                required: "Recommended".to_string(),
                repeatable: "No".to_string(),
                internal_use_only: "No".to_string(),
                defined_by: "uploader".to_string(),
                edit_access: "uploader".to_string(),
                definition: String::new(),
                accepted_values: "String, plain text".to_string(),
                usage_notes: String::new(),
                example: vec![],
            },
            SchemaField {
                field: "collection".to_string(),
                label: "Collection".to_string(),
                required: "Yes".to_string(),
                repeatable: "Yes".to_string(),
                internal_use_only: "No".to_string(),
                defined_by: "uploader".to_string(),
                edit_access: "uploader".to_string(),
                definition: String::new(),
                accepted_values: "Identifier".to_string(),
                usage_notes: String::new(),
                example: vec![],
            },
            SchemaField {
                field: "mediatype".to_string(),
                label: "Media Type".to_string(),
                required: "Yes".to_string(),
                repeatable: "No".to_string(),
                internal_use_only: "No".to_string(),
                defined_by: "uploader".to_string(),
                edit_access: "uploader".to_string(),
                definition: String::new(),
                accepted_values: "texts\netree\naudio".to_string(),
                usage_notes: String::new(),
                example: vec![],
            },
            SchemaField {
                field: "date".to_string(),
                label: "Date".to_string(),
                required: "No".to_string(),
                repeatable: "No".to_string(),
                internal_use_only: "No".to_string(),
                defined_by: "uploader".to_string(),
                edit_access: "uploader".to_string(),
                definition: String::new(),
                accepted_values: "YYYY-MM-DD".to_string(),
                usage_notes: String::new(),
                example: vec![],
            },
            SchemaField {
                field: "old_field".to_string(),
                label: "Old Field".to_string(),
                required: "Deprecated".to_string(),
                repeatable: "No".to_string(),
                internal_use_only: "Yes".to_string(),
                defined_by: "IA software".to_string(),
                edit_access: "IA admin".to_string(),
                definition: String::new(),
                accepted_values: String::new(),
                usage_notes: String::new(),
                example: vec![],
            },
        ]
    }

    #[test]
    fn clean_item_has_no_findings() {
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "title": "Test Item",
            "collection": "test-collection",
            "mediatype": "texts",
            "date": "2024-01-15"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        assert!(result.is_clean(), "findings: {:?}", result.findings);
    }

    #[test]
    fn detects_missing_required_field() {
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "title": "Test"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        let required_findings: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.message.contains("missing"))
            .collect();
        assert!(required_findings.len() >= 2); // collection and mediatype
        assert!(required_findings.iter().any(|f| f.field == "collection"));
        assert!(required_findings.iter().any(|f| f.field == "mediatype"));
    }

    #[test]
    fn detects_missing_recommended_field() {
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "collection": "test",
            "mediatype": "texts"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        let rec = result
            .findings
            .iter()
            .find(|f| f.field == "title" && f.message.contains("recommended"));
        assert!(rec.is_some());
        assert_eq!(rec.unwrap().severity, Severity::Warning);
    }

    #[test]
    fn detects_unexpected_type() {
        // mediatype as a number instead of a string
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "collection": "test",
            "mediatype": 42,
            "title": "Test"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        let type_finding = result
            .findings
            .iter()
            .find(|f| f.field == "mediatype" && f.message.contains("unexpected type"));
        assert!(type_finding.is_some());
        assert_eq!(type_finding.unwrap().severity, Severity::Warning);
    }

    #[test]
    fn detects_repeatability_violation() {
        // date is non-repeatable but has multiple values
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "collection": "test",
            "mediatype": "texts",
            "title": "Test",
            "date": ["2004", "December 6, 2004", "December 6, 2004"]
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        let rep = result
            .findings
            .iter()
            .find(|f| f.field == "date" && f.message.contains("not repeatable"));
        assert!(rep.is_some());
        assert_eq!(rep.unwrap().severity, Severity::Warning);
    }

    #[test]
    fn detects_deprecated_field() {
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "collection": "test",
            "mediatype": "texts",
            "title": "Test",
            "old_field": "some value"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        let dep = result
            .findings
            .iter()
            .find(|f| f.field == "old_field" && f.message.contains("deprecated"));
        assert!(dep.is_some());
        assert_eq!(dep.unwrap().severity, Severity::Info);
    }

    #[test]
    fn detects_unknown_field() {
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "collection": "test",
            "mediatype": "texts",
            "title": "Test",
            "totally_made_up": "value"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        let unknown = result
            .findings
            .iter()
            .find(|f| f.field == "totally_made_up" && f.message.contains("not in schema"));
        assert!(unknown.is_some());
        assert_eq!(unknown.unwrap().severity, Severity::Info);
    }

    #[test]
    fn findings_sorted_by_severity_then_field() {
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "title": "Test",
            "old_field": "deprecated val",
            "zzz_unknown": "val"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        // Should have: errors first (missing collection, mediatype),
        // then warnings, then info (deprecated, unknown).
        let severities: Vec<_> = result.findings.iter().map(|f| &f.severity).collect();
        for i in 1..severities.len() {
            let ord = |s: &Severity| match s {
                Severity::Error => 0,
                Severity::Warning => 1,
                Severity::Info => 2,
            };
            assert!(ord(severities[i - 1]) <= ord(severities[i]));
        }
    }

    #[test]
    fn audit_result_count() {
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "title": "Test"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        assert!(result.count(&Severity::Error) >= 2); // missing collection + mediatype
        assert!(!result.is_clean());
    }

    #[test]
    fn collection_repeatable_multiple_values_ok() {
        // collection is repeatable=Yes, so multiple values is fine
        let metadata: MetadataFields = serde_json::from_value(serde_json::json!({
            "identifier": "test-item",
            "collection": ["coll1", "coll2", "coll3"],
            "mediatype": "texts",
            "title": "Test"
        }))
        .unwrap();

        let result = audit_item("test-item", &metadata, &test_schema());
        let rep = result
            .findings
            .iter()
            .any(|f| f.field == "collection" && f.message.contains("not repeatable"));
        assert!(!rep, "collection is repeatable — should not flag");
    }

    /// A schema field with the given name, `required` and `repeatable`
    /// values.
    fn field(name: &str, required: &str, repeatable: &str) -> SchemaField {
        SchemaField {
            field: name.to_string(),
            label: name.to_string(),
            required: required.to_string(),
            repeatable: repeatable.to_string(),
            internal_use_only: "No".to_string(),
            defined_by: "uploader".to_string(),
            edit_access: "uploader".to_string(),
            definition: String::new(),
            accepted_values: String::new(),
            usage_notes: String::new(),
            example: vec![],
        }
    }

    fn audit_json(metadata: serde_json::Value, schema: &[SchemaField]) -> Vec<Finding> {
        let metadata: MetadataFields = serde_json::from_value(metadata).unwrap();
        audit_item("test-item", &metadata, schema).findings
    }

    /// `(field, kind, message)` of each finding, for comparisons.
    fn summary(findings: &[Finding]) -> Vec<(String, FindingKind, String)> {
        findings
            .iter()
            .map(|f| (f.field.clone(), f.kind.clone(), f.message.clone()))
            .collect()
    }

    #[test]
    fn untyped_field_repeatability_violation() {
        let findings = audit_json(
            serde_json::json!({"volume": ["1", "2"]}),
            &[field("volume", "No", "No")],
        );
        assert_eq!(
            summary(&findings),
            vec![(
                "volume".to_string(),
                FindingKind::Repeatability,
                "not repeatable but has 2 values".to_string()
            )]
        );
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn untyped_field_checked_like_typed_field() {
        // The same values under a typed field (`title`) and an untyped one
        // (`volume`) with the same schema entry give the same findings.
        for value in [
            serde_json::json!(["a", "b", "c"]),
            serde_json::json!(42),
            serde_json::json!({"k": "v"}),
            serde_json::json!(["a", 1]),
            serde_json::json!("a"),
            serde_json::json!(["a"]),
        ] {
            let typed = audit_json(
                serde_json::json!({"title": value.clone()}),
                &[field("title", "No", "No")],
            );
            let untyped = audit_json(
                serde_json::json!({"volume": value.clone()}),
                &[field("volume", "No", "No")],
            );
            let rename = |s: Vec<(String, FindingKind, String)>| {
                s.into_iter()
                    .map(|(_, kind, message)| (kind, message))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                rename(summary(&typed)),
                rename(summary(&untyped)),
                "value {value}"
            );
        }
    }

    #[test]
    fn untyped_field_unexpected_type() {
        let findings = audit_json(
            serde_json::json!({"volume": 3}),
            &[field("volume", "No", "Yes")],
        );
        assert_eq!(
            summary(&findings),
            vec![(
                "volume".to_string(),
                FindingKind::TypeMismatch,
                "unexpected type: got number (3)".to_string()
            )]
        );
    }

    #[test]
    fn untyped_repeatable_field_multiple_values_ok() {
        let findings = audit_json(
            serde_json::json!({"contributor": ["a", "b"], "volume": "1"}),
            &[
                field("contributor", "No", "Yes"),
                field("volume", "No", "No"),
            ],
        );
        assert!(findings.is_empty(), "findings: {findings:?}");
    }

    #[test]
    fn untyped_field_deprecated_unknown_and_required_still_reported() {
        let findings = audit_json(
            serde_json::json!({"old_thing": ["x", "y"], "made_up": ["p", "q"]}),
            &[
                field("old_thing", "Deprecated", "No"),
                field("scanner", "Recommended", "No"),
            ],
        );
        assert_eq!(
            summary(&findings),
            vec![
                (
                    "old_thing".to_string(),
                    FindingKind::Repeatability,
                    "not repeatable but has 2 values".to_string()
                ),
                (
                    "scanner".to_string(),
                    FindingKind::MissingRequired,
                    "missing recommended field".to_string()
                ),
                (
                    "made_up".to_string(),
                    FindingKind::Unknown,
                    "field not in schema".to_string()
                ),
                (
                    "old_thing".to_string(),
                    FindingKind::Deprecated,
                    "deprecated field is present".to_string()
                ),
            ]
        );
    }

    #[test]
    fn null_field_is_absent_for_typed_and_untyped_fields() {
        // A typed field deserializes `null` to `None`; an untyped field's
        // `null` is treated the same way, so neither is a type mismatch and
        // both count as missing.
        let findings = audit_json(
            serde_json::json!({"title": null, "volume": null, "made_up": null}),
            &[
                field("title", "Recommended", "No"),
                field("volume", "Recommended", "No"),
            ],
        );
        assert_eq!(
            summary(&findings),
            vec![
                (
                    "title".to_string(),
                    FindingKind::MissingRequired,
                    "missing recommended field".to_string()
                ),
                (
                    "volume".to_string(),
                    FindingKind::MissingRequired,
                    "missing recommended field".to_string()
                ),
            ]
        );
    }

    #[test]
    fn describe_value_formats_types() {
        assert_eq!(describe_value(&serde_json::json!(null)), "null");
        assert_eq!(describe_value(&serde_json::json!(true)), "boolean (true)");
        assert_eq!(describe_value(&serde_json::json!(42)), "number (42)");
        assert_eq!(
            describe_value(&serde_json::json!([1, 2, 3])),
            "array of 3 elements"
        );
        assert_eq!(
            describe_value(&serde_json::json!({"a": 1})),
            "object with 1 key"
        );
    }
}
