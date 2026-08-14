//! Deterministic MCP input-schema → OpenRouter function-parameter schema
//! normalizer and a small local JSON-schema validator.
//!
//! This module is the single source of truth for schema translation used by
//! the OpenRouter e2e suite: every runtime `Tool` definition is normalized
//! here, and model-generated arguments are validated against the normalized
//! schema before they are ever passed to an MCP server.
//!
//! All tests in this module are offline and use local JSON fixtures only.

use serde_json::{Map, Value};

/// Result of normalizing one MCP tool schema.
#[derive(Debug, Clone, Default)]
pub struct Normalized {
    /// The OpenRouter-parameter schema (JSON Schema, `type: object` at the
    /// root, no `$schema`/`$defs`/`$ref` left behind).
    pub schema: Value,
    /// Human-readable diagnostics for every unsupported construct that was
    /// dropped or simplified. Empty when the schema used only supported
    /// constructs.
    pub diagnostics: Vec<String>,
}

/// Keywords handled explicitly by the normalizer. Everything else that
/// appears in a schema node is recorded as an unsupported diagnostic and
/// dropped, so a model never sees a construct we cannot validate.
const SUPPORTED: &[&str] = &[
    "type",
    "properties",
    "required",
    "items",
    "description",
    "default",
    "enum",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "additionalProperties",
    "format",
];

/// Keywords that are silently consumed (JSON Schema meta / reference
/// machinery) rather than reported as unsupported.
const META: &[&str] = &["$schema", "$defs", "title", "$ref"];

/// Normalize an MCP tool input schema (as produced by
/// `rmcp::model::Tool::schema_as_json_value`) into a function-parameters
/// schema for OpenRouter.
///
/// Guarantees:
/// - the result is always `type: object` with a `properties` map;
/// - `$defs`/`$ref` are resolved recursively (local `#/$defs/NAME` refs);
/// - nullable `type: ["T", "null"]` collapses to `T`;
/// - `properties`, `required`, `description`, `default`, `enum`,
///   `minimum`, `maximum`, `minItems`, `maxItems` and declared
///   `additionalProperties` are preserved verbatim;
/// - every object node declares `additionalProperties: false` unless the
///   schema already declared it, so a model cannot invent parameters;
/// - any other keyword is dropped and reported in `diagnostics`.
pub fn normalize_tool_schema(mcp: &Value) -> Normalized {
    let defs = match mcp.get("$defs").and_then(Value::as_object) {
        Some(defs) => defs.clone(),
        None => Map::new(),
    };
    let mut normalized = Normalized::default();
    let mut root = normalize_node(mcp, &defs, "$", &mut normalized);
    match root {
        Value::Object(_) => {}
        _ => {
            normalized
                .diagnostics
                .push("$: schema root is not an object; replaced with {}.".to_string());
            root = Value::Object(Map::new());
        }
    }
    let obj = root.as_object_mut().expect("root is an object");
    if !obj.contains_key("type") {
        obj.insert("type".to_string(), Value::String("object".to_string()));
        normalized
            .diagnostics
            .push("$: missing type at root; assumed object.".to_string());
    }
    if !obj.contains_key("properties") {
        obj.insert("properties".to_string(), Value::Object(Map::new()));
    }
    // Root-level additionalProperties policy.
    match obj.get("additionalProperties") {
        None => {
            obj.insert("additionalProperties".to_string(), Value::Bool(false));
        }
        Some(Value::Bool(true)) => {
            normalized.diagnostics.push(
                "$: additionalProperties: true at root is preserved but weakens the ".to_string()
                    + "argument contract.",
            );
        }
        _ => {}
    }
    normalized.schema = Value::Object(obj.clone());
    normalized
}

/// Normalize one schema node, resolving `$defs` refs and recursing into
/// object/array constructs.
fn normalize_node(
    node: &Value,
    defs: &Map<String, Value>,
    path: &str,
    out: &mut Normalized,
) -> Value {
    let Some(obj) = node.as_object() else {
        // Non-object schema nodes (e.g. bare `true`) are left untouched.
        return node.clone();
    };
    let mut result = Map::new();

    // Resolve local refs first; anything else is unsupported.
    if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
        if let Some(name) = reference.strip_prefix("#/$defs/") {
            if let Some(target) = defs.get(name) {
                return normalize_node(target, defs, &format!("{path}.$ref:{name}"), out);
            }
            out.diagnostics.push(format!(
                "{path}: unresolvable $ref {reference:?}; node dropped."
            ));
            return Value::Object(Map::new());
        }
        out.diagnostics.push(format!(
            "{path}: non-local $ref {reference:?} is unsupported; node dropped."
        ));
        return Value::Object(Map::new());
    }

    // Collapse nullable type arrays: ["T", "null"] → T.
    if let Some(types) = obj.get("type").and_then(Value::as_array) {
        let non_null: Vec<&str> = types
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect();
        if non_null.len() == 1 {
            result.insert("type".to_string(), Value::String(non_null[0].to_string()));
        } else {
            if !non_null.is_empty() {
                out.diagnostics.push(format!(
                    "{path}: type array {types:?} is unsupported (only [T, null] is handled); \
                     keeping the first non-null type."
                ));
            }
            if let Some(first) = non_null.first() {
                result.insert("type".to_string(), Value::String(first.to_string()));
            }
        }
    }

    for (key, value) in obj {
        if SUPPORTED.contains(&key.as_str()) {
            if key == "type" {
                // Array form was already collapsed above; a scalar type is
                // inserted here. Never overwrite a collapsed result.
                if !result.contains_key("type") {
                    result.insert("type".to_string(), value.clone());
                }
                continue;
            }
            let normalized_value = match key.as_str() {
                "properties" => normalize_object_map(value, defs, path, out),
                "items" | "additionalProperties" => {
                    normalize_node(value, defs, &format!("{path}.{key}"), out)
                }
                _ => value.clone(),
            };
            result.insert(key.clone(), normalized_value);
        } else if !META.contains(&key.as_str()) {
            out.diagnostics
                .push(format!("{path}: unsupported keyword {key:?} dropped."));
        }
    }

    // additionalProperties policy for every object node.
    if result.get("type").and_then(Value::as_str) == Some("object")
        || result.contains_key("properties")
    {
        match result.get("additionalProperties") {
            None => {
                result.insert("additionalProperties".to_string(), Value::Bool(false));
            }
            Some(Value::Bool(true)) => {
                out.diagnostics.push(format!(
                    "{path}: additionalProperties: true is preserved but weakens the argument                      contract."
                ));
            }
            _ => {}
        }
    }

    Value::Object(result)
}

fn normalize_object_map(
    map: &Value,
    defs: &Map<String, Value>,
    path: &str,
    out: &mut Normalized,
) -> Value {
    let Some(object) = map.as_object() else {
        out.diagnostics
            .push(format!("{path}: properties is not an object; dropped."));
        return Value::Object(Map::new());
    };
    let mut result = Map::new();
    for (name, schema) in object {
        result.insert(
            name.clone(),
            normalize_node(schema, defs, &format!("{path}.properties.{name}"), out),
        );
    }
    Value::Object(result)
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// Validate `value` against a (normalized) JSON schema node.
///
/// Deterministic and dependency-free. Implements the subset of JSON Schema
/// that all repository schemas need after normalization: `required`,
/// JSON types (including `["T", "null"]` arrays), object properties with
/// `additionalProperties` (bool or schema), array `items` with
/// `minItems`/`maxItems`, `enum`, and numeric `minimum`/`maximum`.
/// `anyOf`/`oneOf` are honored as union (first matching branch wins) so raw
/// un-normalized fixtures can also be validated. Unknown keywords are
/// ignored.
///
/// Returns every violation found (never stops at the first one), or `Ok`
/// when the value satisfies the schema.
pub fn validate(value: &Value, schema: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    validate_node(value, schema, "$", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_node(value: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(node) = schema.as_object() else {
        // Bare boolean or otherwise unconstrained schema: accept.
        return;
    };

    // Union semantics for un-normalized fixtures.
    if let Some(branches) = node
        .get("anyOf")
        .or_else(|| node.get("oneOf"))
        .and_then(Value::as_array)
    {
        if branches.iter().any(|branch| {
            let mut branch_errors = Vec::new();
            validate_node(value, branch, path, &mut branch_errors);
            branch_errors.is_empty()
        }) {
            return;
        }
        errors.push(format!(
            "{path}: value does not match any union branch: {}",
            describe(value)
        ));
        return;
    }

    if let Some(types) = node.get("type")
        && !matches_type(value, types)
    {
        errors.push(format!(
            "{path}: expected {}, got {}",
            describe_types(types),
            describe(value)
        ));
        // Type mismatches short-circuit deeper checks for this node.
        return;
    }

    if let Some(enum_values) = node.get("enum").and_then(Value::as_array)
        && !enum_values.contains(value)
    {
        errors.push(format!(
            "{path}: value {} is not one of {}",
            describe(value),
            serde_json::to_string(&enum_values).unwrap_or_default()
        ));
    }

    if let Some(minimum) = node.get("minimum").and_then(Value::as_f64)
        && let Some(number) = value.as_f64()
        && number < minimum
    {
        errors.push(format!("{path}: {number} is below minimum {minimum}"));
    }
    if let Some(maximum) = node.get("maximum").and_then(Value::as_f64)
        && let Some(number) = value.as_f64()
        && number > maximum
    {
        errors.push(format!("{path}: {number} is above maximum {maximum}"));
    }

    match value {
        Value::Object(fields) => {
            if let Some(required) = node.get("required").and_then(Value::as_array) {
                for name in required {
                    if let Some(name) = name.as_str()
                        && !fields.contains_key(name)
                    {
                        errors.push(format!("{path}: missing required property {name:?}"));
                    }
                }
            }
            if let Some(properties) = node.get("properties").and_then(Value::as_object) {
                for (name, subschema) in properties {
                    if let Some(child) = fields.get(name) {
                        validate_node(child, subschema, &format!("{path}.{name}"), errors);
                    }
                }
            }
            match node.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    for name in fields.keys() {
                        let declared = properties_declare(node, name);
                        if !declared {
                            errors.push(format!(
                                "{path}: unexpected property {name:?} (additionalProperties is                                  false)"
                            ));
                        }
                    }
                }
                Some(Value::Bool(true)) | None => {}
                Some(subschema @ Value::Object(_)) => {
                    for (name, child) in fields {
                        let declared = properties_declare(node, name);
                        if !declared {
                            validate_node(child, subschema, &format!("{path}.{name}"), errors);
                        }
                    }
                }
                Some(other) => errors.push(format!(
                    "{path}: unsupported additionalProperties value {other:?}"
                )),
            }
        }
        Value::Array(items) => {
            if let Some(min_items) = node.get("minItems").and_then(Value::as_u64)
                && (items.len() as u64) < min_items
            {
                errors.push(format!(
                    "{path}: expected at least {min_items} items, got {}",
                    items.len()
                ));
            }
            if let Some(max_items) = node.get("maxItems").and_then(Value::as_u64)
                && (items.len() as u64) > max_items
            {
                errors.push(format!(
                    "{path}: expected at most {max_items} items, got {}",
                    items.len()
                ));
            }
            if let Some(items_schema) = node.get("items") {
                for (index, item) in items.iter().enumerate() {
                    validate_node(item, items_schema, &format!("{path}[{index}]"), errors);
                }
            }
        }
        _ => {}
    }
}

fn properties_declare(node: &serde_json::Map<String, Value>, name: &str) -> bool {
    node.get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key(name))
}

fn matches_type(value: &Value, types: &Value) -> bool {
    let single = |t: &str| match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        _ => true, // unknown type names do not constrain
    };
    match types {
        Value::String(t) => single(t),
        Value::Array(list) => list.iter().filter_map(Value::as_str).any(single),
        _ => true,
    }
}

fn describe_types(types: &Value) -> String {
    match types {
        Value::String(t) => format!("{t:?}"),
        Value::Array(list) => list
            .iter()
            .filter_map(Value::as_str)
            .map(|t| format!("{t:?}"))
            .collect::<Vec<_>>()
            .join(" or "),
        _ => "a valid JSON type".to_string(),
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::String(s) => {
            let shown: String = s.chars().take(40).collect();
            format!(
                "string {shown:?}{}",
                if s.chars().count() > 40 { "…" } else { "" }
            )
        }
        Value::Null => "null".to_string(),
        Value::Bool(b) => format!("boolean {b}"),
        Value::Number(n) => format!("number {n}"),
        Value::Array(a) => format!("array of {} items", a.len()),
        Value::Object(o) => format!("object with {} properties", o.len()),
    }
}

// ---------------------------------------------------------------------------
// Offline unit tests (local JSON fixtures only)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn norm(fixture: Value) -> Normalized {
        normalize_tool_schema(&fixture)
    }

    fn validate_ok(value: Value, schema: &Value) {
        match validate(&value, schema) {
            Ok(()) => {}
            Err(errors) => panic!("expected valid, got errors: {errors:?}"),
        }
    }

    fn validate_err(value: Value, schema: &Value, expected: &[&str]) {
        let errors = validate(&value, schema).expect_err("expected invalid");
        for needle in expected {
            assert!(
                errors.iter().any(|e| e.contains(needle)),
                "expected an error containing {needle:?} in {errors:?}"
            );
        }
    }

    // -- normalizer: fixtures ---------------------------------------------

    #[test]
    fn normalizes_plain_object_schema() {
        let fixture = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" },
                "head": {
                    "type": ["integer", "null"],
                    "default": null,
                    "format": "uint32",
                    "minimum": 0
                }
            },
            "required": ["path"]
        });
        let out = norm(fixture);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let schema = out.schema;
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(
            schema["properties"]["head"]["type"], "integer",
            "null collapsed"
        );
        assert_eq!(schema["properties"]["head"]["default"], Value::Null);
        assert_eq!(schema["properties"]["head"]["minimum"], 0);
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["additionalProperties"], false);
        assert!(!schema.as_object().unwrap().contains_key("$schema"));
        assert!(
            schema["properties"]["head"]
                .as_object()
                .unwrap()
                .get("additionalProperties")
                .is_none(),
            "scalar nodes carry no additionalProperties"
        );
    }

    #[test]
    fn resolves_local_defs_refs_nested() {
        let fixture = json!({
            "$defs": {
                "EditOperation": {
                    "type": "object",
                    "properties": {
                        "oldText": { "type": "string" },
                        "newText": { "type": "string" }
                    },
                    "required": ["oldText", "newText"]
                }
            },
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/EditOperation" }
                }
            },
            "required": ["edits"]
        });
        let out = norm(fixture);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let items = &out.schema["properties"]["edits"]["items"];
        assert_eq!(items["type"], "object");
        assert_eq!(items["properties"]["oldText"]["type"], "string");
        assert_eq!(items["required"], json!(["oldText", "newText"]));
        assert!(!out.schema.as_object().unwrap().contains_key("$defs"));
        assert!(!items.as_object().unwrap().contains_key("$ref"));
    }

    #[test]
    fn preserves_enums_min_max_and_list_bounds() {
        let fixture = json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["name", "size", "mtime"]
                },
                "count": { "type": "integer", "minimum": 1, "maximum": 99 },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": 5
                }
            },
            "required": ["mode", "count"]
        });
        let out = norm(fixture);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert_eq!(
            out.schema["properties"]["mode"]["enum"],
            json!(["name", "size", "mtime"])
        );
        assert_eq!(out.schema["properties"]["count"]["minimum"], 1);
        assert_eq!(out.schema["properties"]["count"]["maximum"], 99);
        assert_eq!(out.schema["properties"]["tags"]["minItems"], 1);
        assert_eq!(out.schema["properties"]["tags"]["maxItems"], 5);
    }

    #[test]
    fn collects_unsupported_keyword_diagnostics() {
        let fixture = json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "pattern": "^[a-z]+$",
                    "minLength": 3,
                    "allOf": [{ "type": "string" }]
                }
            }
        });
        let out = norm(fixture);
        let messages = out.diagnostics.join("\n");
        for keyword in ["pattern", "minLength", "allOf"] {
            assert!(
                messages.contains(keyword),
                "diagnostic for {keyword} missing: {messages}"
            );
        }
        // Dropped keywords do not reach the model schema.
        let name = &out.schema["properties"]["name"];
        assert!(!name.as_object().unwrap().contains_key("pattern"));
        assert!(!name.as_object().unwrap().contains_key("allOf"));
        assert_eq!(name["type"], "string");
    }

    #[test]
    fn reports_non_local_refs_and_missing_defs() {
        let fixture = json!({
            "type": "object",
            "properties": {
                "bad": { "$ref": "#/components/schemas/X" },
                "missing": { "$ref": "#/$defs/Nope" }
            }
        });
        let out = norm(fixture);
        let messages = out.diagnostics.join("\n");
        assert!(messages.contains("non-local $ref"), "{messages}");
        assert!(messages.contains("unresolvable $ref"), "{messages}");
        assert_eq!(out.schema["properties"]["bad"], json!({}));
    }

    #[test]
    fn empty_object_schema_and_no_properties() {
        for fixture in [
            json!({ "type": "object", "properties": {} }),
            json!({}),
            json!({ "properties": {} }),
        ] {
            let out = norm(fixture);
            assert_eq!(out.schema["type"], "object");
            assert_eq!(out.schema["properties"], json!({}));
            assert_eq!(out.schema["additionalProperties"], false);
        }
    }

    #[test]
    fn preserves_declared_additional_properties_schema() {
        let fixture = json!({
            "type": "object",
            "properties": { "known": { "type": "string" } },
            "additionalProperties": { "type": "integer" }
        });
        let out = norm(fixture);
        assert_eq!(
            out.schema["additionalProperties"],
            json!({ "type": "integer" })
        );
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    }

    #[test]
    fn array_items_are_recursively_normalized() {
        let fixture = json!({
            "type": "object",
            "properties": {
                "matrix": {
                    "type": "array",
                    "items": {
                        "type": "array",
                        "items": { "type": "integer" }
                    }
                }
            }
        });
        let out = norm(fixture);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert_eq!(out.schema["properties"]["matrix"]["type"], "array");
        let items = &out.schema["properties"]["matrix"]["items"];
        assert_eq!(items["type"], "array");
        assert_eq!(items["items"]["type"], "integer");
    }

    #[test]
    fn multi_type_arrays_are_diagnosed() {
        let fixture = json!({
            "type": "object",
            "properties": { "x": { "type": ["string", "integer"] } }
        });
        let out = norm(fixture);
        assert!(
            out.diagnostics.join("\n").contains("unsupported"),
            "{:?}",
            out.diagnostics
        );
        assert_eq!(out.schema["properties"]["x"]["type"], "string");
    }

    // -- validator: fixtures ----------------------------------------------

    #[test]
    fn validator_checks_required_and_types() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "count": { "type": "integer" },
                "flag": { "type": "boolean" },
                "maybe": { "type": ["string", "null"] }
            },
            "required": ["path"],
            "additionalProperties": false
        });
        validate_ok(
            json!({ "path": "a.txt", "count": 3, "flag": true, "maybe": null }),
            &schema,
        );
        validate_ok(json!({ "path": "a.txt" }), &schema);
        validate_err(json!({}), &schema, &["missing required property \"path\""]);
        validate_err(
            json!({ "path": 42 }),
            &schema,
            &["expected \"string\", got number 42"],
        );
        validate_err(
            json!({ "path": "a", "count": 1.5 }),
            &schema,
            &["expected \"integer\", got number 1.5"],
        );
        validate_err(
            json!({ "path": "a", "extra": 1 }),
            &schema,
            &["unexpected property \"extra\""],
        );
    }

    #[test]
    fn validator_checks_nested_objects_and_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string" },
                            "newText": { "type": "string" }
                        },
                        "required": ["oldText", "newText"],
                        "additionalProperties": false
                    },
                    "minItems": 1,
                    "maxItems": 3
                }
            },
            "required": ["edits"],
            "additionalProperties": false
        });
        validate_ok(
            json!({ "edits": [{ "oldText": "a", "newText": "b" }, { "oldText": "b", "newText": "c" }] }),
            &schema,
        );
        validate_err(
            json!({ "edits": [] }),
            &schema,
            &["expected at least 1 items"],
        );
        validate_err(
            json!({ "edits": [{ "oldText": "a" }] }),
            &schema,
            &["missing required property \"newText\""],
        );
        validate_err(
            json!({ "edits": [{ "oldText": "a", "newText": "b", "surprise": true }] }),
            &schema,
            &["unexpected property \"surprise\""],
        );
        validate_err(
            json!({ "edits": [{ "oldText": 1, "newText": "b" }] }),
            &schema,
            &["oldText: expected \"string\""],
        );
    }

    #[test]
    fn validator_checks_enum_and_numeric_bounds() {
        let schema = json!({
            "type": "object",
            "properties": {
                "sort": { "type": "string", "enum": ["name", "size"] },
                "limit": { "type": "integer", "minimum": 1, "maximum": 999999 },
                "index": { "type": "integer", "minimum": 0 }
            },
            "required": ["sort"],
            "additionalProperties": false
        });
        validate_ok(json!({ "sort": "size", "limit": 5, "index": 0 }), &schema);
        validate_err(json!({ "sort": "mtime" }), &schema, &["not one of"]);
        validate_err(
            json!({ "sort": "name", "limit": 0 }),
            &schema,
            &["below minimum 1"],
        );
        validate_err(
            json!({ "sort": "name", "limit": 1000000 }),
            &schema,
            &["above maximum 999999"],
        );
        validate_err(
            json!({ "sort": "name", "index": -1 }),
            &schema,
            &["below minimum 0"],
        );
    }

    #[test]
    fn validator_union_branches_and_additional_property_schema() {
        let schema = json!({
            "type": "object",
            "properties": { "known": { "type": "string" } },
            "additionalProperties": { "type": "integer" }
        });
        validate_ok(json!({ "known": "a", "extra": 7 }), &schema);
        validate_err(
            json!({ "known": "a", "extra": "no" }),
            &schema,
            &["expected \"integer\""],
        );

        let union = json!({
            "type": "object",
            "properties": { "v": { "anyOf": [{ "type": "string" }, { "type": "integer" }] } },
            "additionalProperties": false
        });
        validate_ok(json!({ "v": "x" }), &union);
        validate_ok(json!({ "v": 3 }), &union);
        validate_err(
            json!({ "v": true }),
            &union,
            &["does not match any union branch"],
        );
    }

    #[test]
    fn validator_accepts_unconstrained_nodes() {
        let schema =
            json!({ "type": "object", "properties": { "x": {} }, "additionalProperties": false });
        validate_ok(json!({ "x": { "anything": [1, 2, "three"] } }), &schema);
        let no_type = json!({ "type": "object", "properties": { "y": { "description": "no type" } }, "additionalProperties": false });
        validate_ok(json!({ "y": null }), &no_type);
        validate_ok(json!({ "y": "anything" }), &no_type);
    }
}
