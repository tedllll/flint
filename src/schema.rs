//! A hand-written subset of JSON Schema, checked locally before an answer is accepted.
//!
//! DeepSeek's JSON mode is `response_format: {"type": "json_object"}`, and all it promises is
//! that the reply parses as JSON — nothing about its shape. `{"day": 3}` parses, and is the
//! wrong answer to a schema that asked for `trading_day`. So the shape is flint's problem: the
//! schema goes into the system prompt, the reply is checked here, and the messages
//! [`Schema::validate`] returns are fed back to the model for a second attempt. That is why
//! every message names a JSON path — the reader is the model.
//!
//! Two decisions live in this file.
//!
//! * **A subset, by hand, rather than a JSON Schema crate.** A crate would validate more and
//!   cost a dependency this repository argues against before accepting one; more to the point,
//!   the value here is agreement rather than coverage, because flint has to be able to tell the
//!   model in plain words why an answer failed. A keyword flint implements badly is worse than
//!   one it refuses to accept at all.
//! * **An unsupported keyword is refused at [`Schema::parse`], never ignored.** Dropping
//!   `oneOf` on the floor would let flint report "valid" for an answer the caller's schema
//!   forbids, which is worse than refusing to start. `pattern` and `format` are refused for
//!   that same reason rather than half-supported: a pattern or a date format flint cannot
//!   enforce exactly is a guarantee that is not there.

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

/// The type names flint recognises. `integer` is not a JSON type — it is a number without a
/// fractional part — and the rest are the JSON value kinds.
const KNOWN_TYPES: [&str; 7] = [
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// A JSON Schema, restricted to what flint validates, parsed and checked up front.
#[derive(Debug, Clone)]
pub struct Schema {
    /// The schema as written, held as a `Value` rather than as a tree of typed rules: it is
    /// rendered back into the prompt verbatim, and [`Schema::parse`] has already proved that
    /// every keyword here has the shape the walk in [`Schema::validate`] expects.
    doc: Value,
}

impl Schema {
    /// The schema as the caller wrote it.
    ///
    /// For the session file and for `debug`: what is recorded and shown has to be the contract
    /// itself rather than this module's reading of it, or a reader cannot see what was agreed to.
    pub fn raw(&self) -> &Value {
        &self.doc
    }

    /// Parse a schema from text (already a JSON value, e.g. read from a file or typed inline).
    ///
    /// This fails loudly if the schema uses a keyword flint does not validate, because
    /// validating around an unknown keyword would mean certifying answers against a schema
    /// nobody checked.
    pub fn parse(text: &str) -> Result<Schema> {
        let doc: Value = serde_json::from_str(text).context("the schema is not valid JSON")?;
        if !doc.is_object() {
            bail!("a schema must be a JSON object, found {}", brief(&doc));
        }
        check_schema(&doc, "$")?;
        Ok(Schema { doc })
    }

    /// A skeleton instance shaped like the schema, for the "an answer looks like this" part of
    /// the prompt. Short on purpose: the point is the shape, not plausible data.
    pub fn example(&self) -> Value {
        example_of(&self.doc)
    }

    /// Check an instance. Empty vector = valid; otherwise one message per problem, each naming
    /// a JSON path, e.g. `$.trading_day: expected string, found number`.
    pub fn validate(&self, value: &Value) -> Vec<String> {
        let mut problems = Vec::new();
        validate_at(&self.doc, value, "$", &mut problems);
        problems
    }

    /// The block that goes into the system prompt: the answer is one JSON value and nothing
    /// else, followed by the schema and an example of the shape.
    pub fn prompt_section(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Reply with a single {} and nothing else: no prose before it, no words after it, \
             and no markdown code fence around it. The answer is that value itself.\n\n",
            self.root_noun()
        ));
        out.push_str("It must satisfy this schema:\n\n```json\n");
        out.push_str(&render(&self.doc));
        out.push_str("\n```\n\nAn answer with the right shape looks like this:\n\n```json\n");
        out.push_str(&render(&self.example()));
        out.push_str("\n```\n");
        out
    }

    /// What the provider's JSON mode can return for this schema. `json_object` is the only
    /// response format DeepSeek supports, so an object root is the norm; the case worth
    /// wording differently is an array root, where calling the answer an object would ask for
    /// something the schema then rejects.
    fn root_noun(&self) -> &'static str {
        match self.doc.get("type").and_then(first_type_name) {
            Some("array") => "json array",
            _ => "json object",
        }
    }
}

/// Walk a schema at parse time, refusing anything flint would not enforce.
///
/// The check is on the *shape of the keywords* as well as their names: `"required": "name"`
/// would otherwise be an instruction flint silently ignores while the caller believes it was
/// applied, which is the same failure as an unknown keyword.
fn check_schema(node: &Value, at: &str) -> Result<()> {
    let Some(rules) = node.as_object() else {
        bail!(
            "{at}: a subschema must be a JSON object, found {}",
            brief(node)
        );
    };
    for (keyword, spec) in rules {
        match keyword.as_str() {
            "type" => check_type(spec, at)?,
            "properties" => {
                let Some(props) = spec.as_object() else {
                    bail!(
                        "{at}.properties: expected an object mapping property names to \
                         subschemas, found {}",
                        brief(spec)
                    );
                };
                for (name, sub) in props {
                    check_schema(sub, &format!("{at}.properties.{name}"))?;
                }
            }
            "required" => {
                let Some(names) = spec.as_array() else {
                    bail!(
                        "{at}.required: expected an array of property names, found {}",
                        brief(spec)
                    );
                };
                if let Some(bad) = names.iter().find(|name| !name.is_string()) {
                    bail!(
                        "{at}.required: every entry must be a property name, found {}",
                        brief(bad)
                    );
                }
            }
            "additionalProperties" => {
                if !spec.is_boolean() {
                    bail!(
                        "{at}.additionalProperties: expected true or false, found {}",
                        brief(spec)
                    );
                }
            }
            "items" => check_schema(spec, &format!("{at}.items"))?,
            "enum" => {
                let Some(values) = spec.as_array() else {
                    bail!(
                        "{at}.enum: expected an array of allowed values, found {}",
                        brief(spec)
                    );
                };
                if values.is_empty() {
                    bail!(
                        "{at}.enum: an empty list of values matches nothing; drop the keyword \
                         to accept any value"
                    );
                }
            }
            "minimum" | "maximum" => {
                if !spec.is_number() {
                    bail!("{at}.{keyword}: expected a number, found {}", brief(spec));
                }
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" => {
                if count_of(spec).is_none() {
                    bail!(
                        "{at}.{keyword}: expected a whole number, found {}",
                        brief(spec)
                    );
                }
            }
            // Annotations constrain nothing, so there is nothing to validate and nothing to
            // refuse: a caller may write them for a person or for another tool to read.
            "description" | "title" | "default" | "examples" | "$schema" | "$comment" => {}
            "pattern" | "format" => bail!(
                "{at}: flint does not validate {keyword:?} — a pattern or a date format it \
                 cannot enforce exactly is not a guarantee; check the answer in your own \
                 program, or drop the keyword"
            ),
            other => bail!(
                "{at}: flint validates a subset of JSON Schema and {other:?} is not in it; \
                 rewrite the schema with type/properties/required/items/enum/\
                 additionalProperties and the length and bound keywords, or check the answer \
                 in your own program"
            ),
        }
    }
    Ok(())
}

/// `type` is a name or a list of names, and every name has to be one flint can check. An
/// unknown name is refused rather than treated as "any value": `"type": "file"` means the
/// caller expects something flint would wave through.
fn check_type(spec: &Value, at: &str) -> Result<()> {
    let names: Vec<&str> = match spec {
        Value::String(name) => vec![name.as_str()],
        Value::Array(list) => {
            if let Some(bad) = list.iter().find(|name| !name.is_string()) {
                bail!(
                    "{at}.type: every entry must be a type name, found {}",
                    brief(bad)
                );
            }
            list.iter().filter_map(Value::as_str).collect()
        }
        _ => bail!(
            "{at}.type: expected a type name or an array of type names, found {}",
            brief(spec)
        ),
    };
    if names.is_empty() {
        bail!(
            "{at}.type: an empty array of types accepts nothing; drop the keyword to accept \
             any type"
        );
    }
    for name in names {
        if !KNOWN_TYPES.contains(&name) {
            bail!(
                "{at}.type: flint does not know the type {name:?}; it knows {}",
                KNOWN_TYPES.join(", ")
            );
        }
    }
    Ok(())
}

/// Check one instance against one subschema, appending a message per problem.
fn validate_at(schema: &Value, value: &Value, path: &str, problems: &mut Vec<String>) {
    // `parse` refused every schema that is not an object, so this arm is unreachable in
    // practice; returning rather than panicking keeps a schema flint already accepted from
    // taking down the turn.
    let Some(rules) = schema.as_object() else {
        return;
    };

    // A value outside `enum` fails the most specific rule the schema states, and every later
    // rule would only repeat that in vaguer words.
    if let Some(allowed) = rules.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) {
            problems.push(format!(
                "{path}: {} is not one of the allowed values ({})",
                brief(value),
                allowed_list(allowed)
            ));
            return;
        }
    }

    // A wrong type makes the keywords below meaningless — `items` on a number, `minLength` on
    // an array — so the type is the one problem worth reporting here.
    let names = type_names(rules);
    if !names.is_empty() && !names.iter().any(|name| type_matches(name, value)) {
        problems.push(format!(
            "{path}: expected {}, found {}",
            join_or(&names),
            kind_of(value)
        ));
        return;
    }

    match value {
        Value::Object(fields) => {
            let props = rules.get("properties").and_then(Value::as_object);
            if let Some(required) = rules.get("required").and_then(Value::as_array) {
                for name in required.iter().filter_map(Value::as_str) {
                    if !fields.contains_key(name) {
                        problems.push(format!("{path}: missing required property {name:?}"));
                    }
                }
            }
            if rules.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
                for name in fields.keys() {
                    if !props.is_some_and(|props| props.contains_key(name)) {
                        problems.push(format!(
                            "{path}: unexpected property {name:?} (additionalProperties is \
                             false)"
                        ));
                    }
                }
            }
            if let Some(props) = props {
                for (name, sub) in props {
                    if let Some(field) = fields.get(name) {
                        validate_at(sub, field, &format!("{path}.{name}"), problems);
                    }
                }
            }
        }
        Value::Array(elements) => {
            let count = elements.len() as u64;
            if let Some(min) = rules.get("minItems").and_then(count_of) {
                if count < min {
                    problems.push(format!(
                        "{path}: expected at least minItems {min}, found {count}"
                    ));
                }
            }
            if let Some(max) = rules.get("maxItems").and_then(count_of) {
                if count > max {
                    problems.push(format!(
                        "{path}: expected at most maxItems {max}, found {count}"
                    ));
                }
            }
            if let Some(items) = rules.get("items") {
                for (index, element) in elements.iter().enumerate() {
                    validate_at(items, element, &format!("{path}[{index}]"), problems);
                }
            }
        }
        Value::String(text) => {
            // Length in characters, not bytes: a byte count would report a schema's limit
            // against the wrong number for anything outside ASCII.
            let count = text.chars().count() as u64;
            if let Some(min) = rules.get("minLength").and_then(count_of) {
                if count < min {
                    problems.push(format!(
                        "{path}: expected at least minLength {min} characters, found {count}"
                    ));
                }
            }
            if let Some(max) = rules.get("maxLength").and_then(count_of) {
                if count > max {
                    problems.push(format!(
                        "{path}: expected at most maxLength {max} characters, found {count}"
                    ));
                }
            }
        }
        Value::Number(number) => {
            if let Some(min) = rules.get("minimum").and_then(Value::as_f64) {
                if number.as_f64().is_some_and(|found| found < min) {
                    problems.push(format!("{path}: {number} is less than the minimum {min}"));
                }
            }
            if let Some(max) = rules.get("maximum").and_then(Value::as_f64) {
                if number.as_f64().is_some_and(|found| found > max) {
                    problems.push(format!(
                        "{path}: {number} is greater than the maximum {max}"
                    ));
                }
            }
        }
        _ => {}
    }
}

/// Build a skeleton instance. Every rule here mirrors one arm of [`validate_at`], so that the
/// example shown to the model is one the schema would accept.
fn example_of(schema: &Value) -> Value {
    let Some(rules) = schema.as_object() else {
        return Value::Null;
    };
    if let Some(first) = rules
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
    {
        return first.clone();
    }
    let kind = rules
        .get("type")
        .and_then(first_type_name)
        // With no `type`, the keyword present is the best guess at the shape the caller means.
        .or_else(|| rules.contains_key("properties").then_some("object"))
        .or_else(|| rules.contains_key("items").then_some("array"))
        .unwrap_or("null");
    match kind {
        "object" => {
            let mut example = Map::new();
            if let Some(props) = rules.get("properties").and_then(Value::as_object) {
                for (name, sub) in props {
                    example.insert(name.clone(), example_of(sub));
                }
            }
            Value::Object(example)
        }
        "array" => match rules.get("items") {
            Some(items) => Value::Array(vec![example_of(items)]),
            None => Value::Array(Vec::new()),
        },
        "string" => {
            let mut text = String::from("string");
            // Padding the placeholder is the cheap way to show a minimum the schema means
            // business about; inventing realistic data is not an example's job.
            if let Some(min) = rules.get("minLength").and_then(count_of) {
                while (text.chars().count() as u64) < min {
                    text.push('x');
                }
            }
            Value::String(text)
        }
        "integer" | "number" => Value::from(0),
        "boolean" => Value::Bool(true),
        _ => Value::Null,
    }
}

/// The type names in a `type` keyword, empty when it is absent. `parse` refused every name
/// flint does not know, so these are always checkable.
fn type_names(rules: &Map<String, Value>) -> Vec<&str> {
    match rules.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// The first type named in a `type` keyword: what the example is shaped like when a schema
/// allows more than one, and `["string", "null"]` should show the string.
fn first_type_name(spec: &Value) -> Option<&str> {
    match spec {
        Value::String(name) => Some(name.as_str()),
        Value::Array(list) => list.iter().find_map(Value::as_str),
        _ => None,
    }
}

fn type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        // JSON has no integer type, so a whole number written as a float — `3.0` — is one.
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value.as_f64().is_some_and(|n| n.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        // Unreachable: `parse` refused every other name.
        _ => false,
    }
}

/// The type name flint reports for a value. A whole number is called a `number` rather than an
/// `integer` so that the message for an integer schema reads `expected integer, found number`.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Read a length or count keyword. JSON has no integer type, so `3.0` counts; a fractional or
/// negative bound is not a length, and `parse` refuses it rather than rounding it.
fn count_of(value: &Value) -> Option<u64> {
    if let Some(count) = value.as_u64() {
        return Some(count);
    }
    let number = value.as_f64()?;
    if number.fract() == 0.0 && number >= 0.0 && number <= u64::MAX as f64 {
        Some(number as u64)
    } else {
        None
    }
}

/// `string`, `string or null`, `a, b or null`: how a union of types reads in a sentence.
fn join_or(names: &[&str]) -> String {
    match names.split_last() {
        None => String::from("any type"),
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}

/// The allowed values, as a list short enough to read inside an error message. A long enum is
/// truncated rather than pasted in full: the model needs to know it guessed wrong, not to have
/// the whole table echoed back at it on every retry.
fn allowed_list(values: &[Value]) -> String {
    const SHOWN: usize = 10;
    let mut text = values
        .iter()
        .take(SHOWN)
        .map(brief)
        .collect::<Vec<_>>()
        .join(", ");
    if values.len() > SHOWN {
        text.push_str(", ...");
    }
    text
}

/// A value short enough to sit inside a message the model will read.
fn brief(value: &Value) -> String {
    const LIMIT: usize = 60;
    let text = value.to_string();
    if text.chars().count() <= LIMIT {
        return text;
    }
    let mut cut: String = text.chars().take(LIMIT).collect();
    cut.push_str("...");
    cut
}

/// Pretty-print a value for the prompt. Both call sites hold values built here, where
/// serialization cannot fail, so the compact form is only a fallback that keeps this total.
fn render(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The tests are the specification of the subset: which keyword is refused, which instance
/// passes, and — because these strings are what the model is asked to fix its answer against —
/// what a message actually says.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(text: &str) -> Schema {
        Schema::parse(text).expect("the test's schema should parse")
    }

    fn check(text: &str, answer: Value) -> Vec<String> {
        schema(text).validate(&answer)
    }

    /// The one message whose path is `path`. Messages are `{path}: {problem}`, and matching on
    /// the colon keeps `$.a` from being found by a search for `$`.
    fn error_at<'a>(problems: &'a [String], path: &str) -> &'a str {
        let wanted = format!("{path}:");
        problems
            .iter()
            .find(|problem| problem.starts_with(&wanted))
            .map(String::as_str)
            .unwrap_or_else(|| panic!("no error at {path} in {problems:?}"))
    }

    #[test]
    fn a_valid_nested_instance_validates_with_no_errors() {
        let text = r#"{
            "type": "object",
            "properties": {
                "trading_day": { "type": "string" },
                "session": {
                    "type": "object",
                    "properties": { "open": { "type": "number" } },
                    "required": ["open"]
                }
            },
            "required": ["trading_day", "session"]
        }"#;
        let problems = check(
            text,
            json!({"trading_day": "2024-01-02", "session": {"open": 9.5}}),
        );
        assert_eq!(problems, Vec::<String>::new());
    }

    #[test]
    fn a_missing_required_property_is_reported_by_name() {
        let text = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["b"]}"#;
        let problems = check(text, json!({"a": "here"}));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0], r#"$: missing required property "b""#);
    }

    #[test]
    fn a_wrong_type_is_reported_with_its_json_path() {
        let text = r#"{"type":"object","properties":{
            "trading_day":{"type":"string"},
            "session":{"type":"object","properties":{"open":{"type":"string"}}}
        }}"#;
        let problems = check(text, json!({"trading_day": 3, "session": {"open": 4}}));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(
            error_at(&problems, "$.trading_day"),
            "$.trading_day: expected string, found number"
        );
        assert_eq!(
            error_at(&problems, "$.session.open"),
            "$.session.open: expected string, found number"
        );
    }

    #[test]
    fn additional_properties_false_refuses_an_extra_property() {
        let text = r#"{"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":false}"#;
        let problems = check(text, json!({"a": "fine", "extra": 1}));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(
            problems[0],
            r#"$: unexpected property "extra" (additionalProperties is false)"#
        );
    }

    #[test]
    fn an_enum_mismatch_is_reported_with_the_allowed_values() {
        let text = r#"{"type":"object","properties":{"status":{"enum":["ok","failed"]}}}"#;
        let problems = check(text, json!({"status": "maybe"}));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(
            error_at(&problems, "$.status"),
            r#"$.status: "maybe" is not one of the allowed values ("ok", "failed")"#
        );
    }

    #[test]
    fn array_items_are_validated_per_element_and_the_path_carries_the_index() {
        let text = r#"{"type":"array","items":{"type":"object",
            "properties":{"n":{"type":"integer"}}}}"#;
        let problems = check(text, json!([{"n": 1}, {"n": "two"}]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(
            error_at(&problems, "$[1].n"),
            "$[1].n: expected integer, found string"
        );
    }

    #[test]
    fn min_length_maximum_and_min_items_are_enforced() {
        let text = r#"{"type":"object","properties":{
            "name":{"type":"string","minLength":3},
            "age":{"type":"integer","maximum":10},
            "tags":{"type":"array","minItems":2}
        }}"#;
        let problems = check(text, json!({"name": "ab", "age": 11, "tags": ["only"]}));
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(
            error_at(&problems, "$.name"),
            "$.name: expected at least minLength 3 characters, found 2"
        );
        assert_eq!(
            error_at(&problems, "$.age"),
            "$.age: 11 is greater than the maximum 10"
        );
        assert_eq!(
            error_at(&problems, "$.tags"),
            "$.tags: expected at least minItems 2, found 1"
        );
    }

    #[test]
    fn minimum_max_length_and_max_items_are_enforced() {
        let text = r#"{"type":"object","properties":{
            "age":{"type":"integer","minimum":18},
            "code":{"type":"string","maxLength":3},
            "tags":{"type":"array","maxItems":1}
        }}"#;
        let problems = check(text, json!({"age": 17, "code": "abcd", "tags": ["a", "b"]}));
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(
            error_at(&problems, "$.age"),
            "$.age: 17 is less than the minimum 18"
        );
        assert_eq!(
            error_at(&problems, "$.code"),
            "$.code: expected at most maxLength 3 characters, found 4"
        );
        assert_eq!(
            error_at(&problems, "$.tags"),
            "$.tags: expected at most maxItems 1, found 2"
        );
    }

    #[test]
    fn a_min_length_counts_characters_and_not_bytes() {
        // Two characters, four bytes: a byte count would call this long enough.
        let problems = check(r#"{"type":"string","minLength":3}"#, json!("éü"));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(
            problems[0],
            "$: expected at least minLength 3 characters, found 2"
        );
    }

    #[test]
    fn a_type_union_accepts_each_member_and_refuses_anything_else() {
        let text = r#"{"type":"object","properties":{"note":{"type":["string","null"]}}}"#;
        assert_eq!(check(text, json!({"note": "text"})), Vec::<String>::new());
        assert_eq!(check(text, json!({"note": null})), Vec::<String>::new());
        let problems = check(text, json!({"note": 7}));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(
            error_at(&problems, "$.note"),
            "$.note: expected string or null, found number"
        );
    }

    #[test]
    fn a_whole_number_written_as_a_decimal_counts_as_an_integer() {
        assert_eq!(
            check(r#"{"type":"integer"}"#, json!(3.0)),
            Vec::<String>::new()
        );
        let problems = check(r#"{"type":"integer"}"#, json!(3.5));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0], "$: expected integer, found number");
    }

    #[test]
    fn parse_refuses_a_keyword_flint_does_not_validate() {
        let problem = Schema::parse(r#"{"type":"object","oneOf":[{"type":"string"}]}"#)
            .expect_err("oneOf is not in the subset");
        let message = problem.to_string();
        assert!(message.contains("oneOf"), "{message}");
        assert!(
            message.contains("type/properties/required/items/enum"),
            "{message}"
        );

        let nested = Schema::parse(r#"{"type":"array","items":{"pattern":"^a"}}"#)
            .expect_err("pattern is not in the subset");
        assert!(nested.to_string().contains("pattern"), "{nested}");

        let formatted = Schema::parse(r#"{"type":"string","format":"date"}"#)
            .expect_err("format is not in the subset");
        assert!(formatted.to_string().contains("format"), "{formatted}");
    }

    #[test]
    fn parse_refuses_a_schema_that_is_not_an_object() {
        let problem = Schema::parse(r#"["string"]"#).expect_err("a schema must be an object");
        assert!(problem.to_string().contains("JSON object"), "{problem}");
    }

    #[test]
    fn parse_refuses_text_that_is_not_json() {
        let problem = Schema::parse("{type: string}").expect_err("that is not JSON");
        assert!(problem.to_string().contains("not valid JSON"), "{problem}");
    }

    #[test]
    fn the_example_has_exactly_the_properties_the_schema_names() {
        let text = r#"{"type":"object","properties":{
            "trading_day":{"type":"string"},
            "count":{"type":"integer"},
            "session":{"type":"object","properties":{"open":{"type":"boolean"}}}
        }}"#;
        let example = schema(text).example();
        let fields = example
            .as_object()
            .expect("the example should be an object");
        let mut names: Vec<&str> = fields.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, ["count", "session", "trading_day"]);
        assert_eq!(example["count"], json!(0));
        assert_eq!(example["session"]["open"], json!(true));
    }

    #[test]
    fn an_array_example_has_one_element_and_an_enum_example_is_its_first_value() {
        let text = r#"{"type":"object","properties":{
            "tags":{"type":"array","items":{"type":"string"}},
            "empty":{"type":"array"}
        }}"#;
        let example = schema(text).example();
        assert_eq!(example["tags"], json!(["string"]));
        assert_eq!(example["empty"], json!([]));
        assert_eq!(
            schema(r#"{"type":"string","enum":["a","b"]}"#).example(),
            json!("a")
        );
    }

    #[test]
    fn the_prompt_section_states_json_and_shows_the_schema_and_the_example() {
        let text = r#"{"type":"object","properties":{"trading_day":{"type":"string"}},
            "required":["trading_day"]}"#;
        let prompt = schema(text).prompt_section();
        assert!(prompt.contains("json"), "{prompt}");
        assert!(prompt.contains("nothing else"), "{prompt}");
        assert!(prompt.contains("no markdown code fence"), "{prompt}");
        // The schema and the example are both shown, and either one could be missing while
        // the prompt still "contains the schema text"; these two keys only appear in the
        // schema, and the assignment only in the example.
        assert!(prompt.contains(r#""properties": {"#), "{prompt}");
        assert!(prompt.contains(r#""required": ["#), "{prompt}");
        assert!(prompt.contains(r#""trading_day": "string""#), "{prompt}");
    }
}
