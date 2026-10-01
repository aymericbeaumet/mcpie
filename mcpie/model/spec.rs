//! Operation descriptions and JSON Schema generation.

use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether an operation only reads upstream state. v1 exposes only [`OperationKind::Read`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Read,
    Write,
}

/// Everything a facade needs to expose one operation: names, prose and schemas.
#[derive(Debug, Clone, Serialize)]
pub struct OperationSpec {
    /// Canonical `snake_case` name, verb first (`list_issues`).
    pub name: String,
    /// Short human title (`List issues`).
    pub title: String,
    /// One or two sentences for help text and tool descriptions.
    pub description: String,
    pub kind: OperationKind,
    /// True when the input accepts `cursor` and the output is a [`Page`].
    pub paginated: bool,
    /// JSON Schema (2020-12, subschemas inlined) for the input object.
    pub input_schema: Value,
    /// JSON Schema for the output value.
    pub output_schema: Value,
}

impl OperationSpec {
    /// A read-only operation whose schemas derive from `I` and `O`.
    pub fn read<I: JsonSchema, O: JsonSchema>(name: &str, title: &str, description: &str) -> Self {
        Self::raw(
            name,
            title,
            description,
            OperationKind::Read,
            schema_of::<I>(),
            schema_of::<O>(),
        )
    }

    /// A write operation whose schemas derive from `I` and `O`.
    pub fn write<I: JsonSchema, O: JsonSchema>(name: &str, title: &str, description: &str) -> Self {
        Self::raw(
            name,
            title,
            description,
            OperationKind::Write,
            schema_of::<I>(),
            schema_of::<O>(),
        )
    }

    /// An operation with hand-provided schemas, for sources that discover their operations at
    /// runtime (such as external MCP servers).
    pub fn raw(
        name: &str,
        title: &str,
        description: &str,
        kind: OperationKind,
        input_schema: Value,
        output_schema: Value,
    ) -> Self {
        let paginated = has_property(&input_schema, "cursor");
        Self {
            name: name.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            kind,
            paginated,
            input_schema,
            output_schema,
        }
    }

    pub fn is_read(&self) -> bool {
        self.kind == OperationKind::Read
    }
}

fn has_property(schema: &Value, name: &str) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key(name))
}

/// Generate the JSON Schema for `T` the way every facade expects it: draft 2020-12, every
/// subschema inlined (no `$ref`/`$defs`), no `$schema` marker and no root title.
pub fn schema_of<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
        })
        .into_generator();
    let mut value = generator.into_root_schema_for::<T>().to_value();
    if let Some(object) = value.as_object_mut() {
        object.remove("title");
    }
    value
}

/// One page of a paginated operation. `next_cursor` is always present so consumers can rely on
/// the key; `null` means the listing is exhausted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, next_cursor: Option<String>) -> Self {
        Self { items, next_cursor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Input {
        /// The text.
        text: String,
        cursor: Option<String>,
        mode: Option<Mode>,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    #[serde(rename_all = "snake_case")]
    enum Mode {
        Fast,
        Slow,
    }

    #[test]
    fn schema_is_inlined_and_untitled() {
        let schema = schema_of::<Input>();
        assert!(schema.get("$schema").is_none());
        assert!(schema.get("title").is_none());
        assert!(schema.get("$defs").is_none());
        let mode = &schema["properties"]["mode"];
        assert!(mode.get("$ref").is_none(), "{mode}");
        assert_eq!(schema["properties"]["text"]["description"], "The text.");
    }

    #[test]
    fn cursor_property_marks_pagination() {
        let spec = OperationSpec::read::<Input, Page<Value>>("list_x", "List x", "Lists x.");
        assert!(spec.paginated);
        let spec = OperationSpec::read::<Mode, Value>("get_x", "Get x", "Gets x.");
        assert!(!spec.paginated);
    }
}
