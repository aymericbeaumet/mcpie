//! The shared projection of an operation's input schema into flat, typed fields.
//!
//! The CLI turns each field into a flag, REST parses query strings through it, and GraphQL
//! builds arguments from it. Keeping one projection means the three facades cannot drift.

use serde_json::{Map, Value};

use super::spec::OperationSpec;

/// Field names that would collide with global CLI flags or facade conventions.
pub const RESERVED_NAMES: &[&str] = &[
    "input", "format", "config", "help", "version", "verbose", "all", "timeout", "set",
];

#[derive(Debug, Clone, PartialEq)]
pub struct InputField {
    pub name: String,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Str,
    I64,
    F64,
    Bool,
    Enum(Vec<EnumVariant>),
    List(Box<FieldKind>),
    /// Anything the flat model cannot express; passed through as a JSON value.
    Json,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub value: String,
    pub description: Option<String>,
}

/// Project the top-level properties of an input object schema.
pub fn project(input_schema: &Value) -> Vec<InputField> {
    let Some(properties) = input_schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let required: Vec<&str> = input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    properties
        .iter()
        .map(|(name, schema)| {
            let default = schema.get("default").cloned();
            InputField {
                name: name.clone(),
                kind: field_kind(schema),
                required: required.contains(&name.as_str()) && default.is_none(),
                default,
                description: schema
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        })
        .collect()
}

/// Classify one property schema.
pub fn field_kind(schema: &Value) -> FieldKind {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return FieldKind::Enum(
            values
                .iter()
                .filter(|v| !v.is_null())
                .map(|v| EnumVariant {
                    value: scalar_string(v),
                    description: None,
                })
                .collect(),
        );
    }
    if let Some(branches) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        let non_null: Vec<&Value> = branches.iter().filter(|b| !is_null_schema(b)).collect();
        if !non_null.is_empty() && non_null.iter().all(|b| b.get("const").is_some()) {
            return FieldKind::Enum(
                non_null
                    .iter()
                    .map(|b| EnumVariant {
                        value: scalar_string(&b["const"]),
                        description: b
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                    .collect(),
            );
        }
        if let [single] = non_null.as_slice() {
            return field_kind(single);
        }
        return FieldKind::Json;
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect(),
        _ => Vec::new(),
    };
    match types.as_slice() {
        ["string"] => FieldKind::Str,
        ["integer"] => FieldKind::I64,
        ["number"] => FieldKind::F64,
        ["boolean"] => FieldKind::Bool,
        ["array"] => FieldKind::List(Box::new(
            schema
                .get("items")
                .map(field_kind)
                .unwrap_or(FieldKind::Json),
        )),
        _ => FieldKind::Json,
    }
}

fn is_null_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
}

fn scalar_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Parse one textual value (a CLI flag or query parameter) into JSON according to the field.
pub fn coerce(kind: &FieldKind, raw: &str) -> Result<Value, String> {
    match kind {
        FieldKind::Str => Ok(Value::String(raw.to_owned())),
        FieldKind::I64 => raw
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("expected an integer, got {raw:?}")),
        FieldKind::F64 => raw
            .trim()
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| format!("expected a number, got {raw:?}")),
        FieldKind::Bool => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ => Err(format!("expected true or false, got {raw:?}")),
        },
        FieldKind::Enum(variants) => variants
            .iter()
            .any(|v| v.value == raw)
            .then(|| Value::String(raw.to_owned()))
            .ok_or_else(|| {
                let allowed: Vec<&str> = variants.iter().map(|v| v.value.as_str()).collect();
                format!("expected one of {}, got {raw:?}", allowed.join(", "))
            }),
        FieldKind::List(inner) => coerce(inner, raw).map(|v| Value::Array(vec![v])),
        FieldKind::Json => serde_json::from_str(raw).map_err(|e| format!("expected JSON: {e}")),
    }
}

/// Parse several textual values for a list field.
pub fn coerce_list(inner: &FieldKind, raws: &[String]) -> Result<Value, String> {
    raws.iter()
        .map(|raw| coerce(inner, raw))
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// Problems that make an operation impossible to expose consistently. Empty means fine.
pub fn lint(spec: &OperationSpec) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(object) = spec.input_schema.as_object() else {
        problems.push(format!("{}: input schema is not an object", spec.name));
        return problems;
    };
    if object.get("type").and_then(Value::as_str) != Some("object") {
        problems.push(format!("{}: input schema must have type object", spec.name));
    }
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(Map::new);
    for (name, schema) in &properties {
        if RESERVED_NAMES.contains(&name.as_str()) {
            problems.push(format!("{}: input field {name:?} is reserved", spec.name));
        }
        if !super::names::is_valid_operation_name(name) {
            problems.push(format!(
                "{}: input field {name:?} must be snake_case",
                spec.name
            ));
        }
        if let FieldKind::List(inner) = field_kind(schema)
            && matches!(*inner, FieldKind::List(_))
        {
            problems.push(format!("{}: input field {name:?} nests arrays", spec.name));
        }
    }
    if spec.paginated {
        let output_ok = spec
            .output_schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|p| p.contains_key("items") && p.contains_key("next_cursor"));
        if !output_ok {
            problems.push(format!(
                "{}: paginated operations must return items and next_cursor",
                spec.name
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::Deserialize;

    use super::*;
    use crate::model::spec::schema_of;

    #[derive(Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Plain {
        Open,
        Closed,
    }

    #[derive(Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Documented {
        /// Still open.
        Open,
        /// Done.
        Closed,
    }

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct Nested {
        a: u32,
    }

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct Input {
        /// Required text.
        text: String,
        count: Option<u32>,
        ratio: Option<f64>,
        #[serde(default)]
        flag: bool,
        state: Option<Plain>,
        status: Option<Documented>,
        tags: Vec<String>,
        ids: Option<Vec<i64>>,
        nested: Option<Nested>,
        #[serde(default = "default_limit")]
        limit: u32,
    }

    fn default_limit() -> u32 {
        30
    }

    fn field<'a>(fields: &'a [InputField], name: &str) -> &'a InputField {
        fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("field {name}"))
    }

    #[test]
    fn projects_every_supported_shape() {
        let schema = schema_of::<Input>();
        let fields = project(&schema);
        assert_eq!(field(&fields, "text").kind, FieldKind::Str);
        assert!(field(&fields, "text").required);
        assert_eq!(
            field(&fields, "text").description.as_deref(),
            Some("Required text.")
        );
        assert_eq!(field(&fields, "count").kind, FieldKind::I64);
        assert!(!field(&fields, "count").required);
        assert_eq!(field(&fields, "ratio").kind, FieldKind::F64);
        assert_eq!(field(&fields, "flag").kind, FieldKind::Bool);
        assert!(
            !field(&fields, "flag").required,
            "fields with defaults are optional"
        );
        match &field(&fields, "state").kind {
            FieldKind::Enum(variants) => assert_eq!(
                variants
                    .iter()
                    .map(|v| v.value.as_str())
                    .collect::<Vec<_>>(),
                ["open", "closed"]
            ),
            other => panic!("state: {other:?}"),
        }
        match &field(&fields, "status").kind {
            FieldKind::Enum(variants) => {
                assert_eq!(variants[0].value, "open");
                assert_eq!(variants[0].description.as_deref(), Some("Still open."));
            }
            other => panic!("status: {other:?}"),
        }
        assert_eq!(
            field(&fields, "tags").kind,
            FieldKind::List(Box::new(FieldKind::Str))
        );
        assert_eq!(
            field(&fields, "ids").kind,
            FieldKind::List(Box::new(FieldKind::I64))
        );
        assert_eq!(field(&fields, "nested").kind, FieldKind::Json);
        assert_eq!(field(&fields, "limit").default, Some(Value::from(30)));
        assert!(!field(&fields, "limit").required);
    }

    #[test]
    fn coerces_text_per_kind() {
        assert_eq!(coerce(&FieldKind::I64, " 42 ").unwrap(), Value::from(42));
        assert!(coerce(&FieldKind::I64, "x").is_err());
        assert_eq!(coerce(&FieldKind::Bool, "yes").unwrap(), Value::Bool(true));
        assert_eq!(
            coerce(&FieldKind::Bool, "false").unwrap(),
            Value::Bool(false)
        );
        let state = FieldKind::Enum(vec![EnumVariant {
            value: "open".into(),
            description: None,
        }]);
        assert!(coerce(&state, "closed").unwrap_err().contains("open"));
        assert_eq!(
            coerce(&FieldKind::Json, r#"{"a":1}"#).unwrap(),
            serde_json::json!({"a": 1})
        );
        assert_eq!(
            coerce_list(&FieldKind::I64, &["1".into(), "2".into()]).unwrap(),
            serde_json::json!([1, 2])
        );
    }

    #[test]
    fn lints_reserved_and_nested_fields() {
        #[derive(Deserialize, JsonSchema)]
        #[allow(dead_code)]
        struct Bad {
            format: String,
            grid: Vec<Vec<u8>>,
        }
        let spec = OperationSpec::read::<Bad, Value>("bad", "Bad", "Bad.");
        let problems = lint(&spec);
        assert!(
            problems.iter().any(|p| p.contains("reserved")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("nests arrays")),
            "{problems:?}"
        );
        let good = OperationSpec::read::<Input, Value>("good", "Good", "Good.");
        assert!(lint(&good).is_empty(), "{:?}", lint(&good));
    }
}
