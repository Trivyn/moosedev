//! Shape repairs for a JSON answer whose schema travelled in the prompt
//! rather than as provider-enforced decoding (the json_schema contract; see
//! `parse_model_json`). Nothing forces the answer's nesting then, and a model
//! can flatten a nested object into its parent. Every repair here is driven by
//! the schema alone, never by the names of any one caller's fields.

use serde_json::{Map, Value};

/// Fold a flattened or tag-keyed variant back into place: the variant's
/// fields flattened into the parent (below), or the variant keyed by its tag
/// (`untag_keyed`).
///
/// Where the schema gives property `P` an object type chosen among variants
/// (`oneOf` / `anyOf`), each tagged by a property with a `const` value, a
/// model may answer `{"P": "<tag>", ...fields}` instead of
/// `{"P": {"<tag key>": "<tag>", ...fields}}`. When `P` holds exactly one
/// variant's tag and every top-level key the schema does not allow at the top
/// level is a property of that variant, those keys move into `P` beside the
/// tag. Returns the repaired value, or `None` when the shape does not fit
/// (nothing is guessed: an unknown key or an unmatched tag leaves the answer
/// to the caller's validation).
///
/// Qwen3.5-9B on badciv answered `{"message":…,"action":"write","file":…,
/// "content":…}` for `{"message":…,"action":{"action":"write",…}}` three
/// times running.
pub fn unflatten(value: &Value, schema: &Value) -> Option<Value> {
    lift_flattened(value, schema).or_else(|| untag_keyed(value, schema))
}

/// The tagged variants a property's schema chooses among, each with its tag
/// key and its fields.
fn tagged_variants(property: &Value) -> Vec<(String, String, &Map<String, Value>)> {
    property
        .get("oneOf")
        .or_else(|| property.get("anyOf"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|variant| {
            let fields = variant.get("properties")?.as_object()?;
            let (tag_key, tag) = fields.iter().find_map(|(key, field)| {
                Some((key.clone(), field.get("const")?.as_str()?.to_string()))
            })?;
            Some((tag_key, tag, fields))
        })
        .collect()
}

/// A variant keyed by its tag: `{"P": {"replace": {"file":…,…}}}` for
/// `{"P": {"action": "replace", "file":…,…}}`, or `{"P": {"apply_fix": 1}}`
/// for `{"P": {"action": "apply_fix", "fix": 1}}` when the variant has exactly
/// one field besides its tag. The nested object must hold that single key and
/// no tag key; an object value's keys must all be the variant's fields.
/// Qwen3.5-9B answered both ways on badciv.
fn untag_keyed(value: &Value, schema: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let properties = schema.get("properties")?.as_object()?;
    for (name, property) in properties {
        let Some(inner) = object.get(name).and_then(Value::as_object) else {
            continue;
        };
        let variants = tagged_variants(property);
        if variants.is_empty() || inner.len() != 1 {
            continue;
        }
        let (key, body) = inner.iter().next()?;
        if variants.iter().any(|(tag_key, _, _)| tag_key == key) {
            continue;
        }
        let Some((tag_key, tag, fields)) = variants.iter().find(|(_, tag, _)| tag == key) else {
            continue;
        };
        let mut nested = Map::new();
        nested.insert(tag_key.clone(), Value::String(tag.clone()));
        match body {
            Value::Object(body) => {
                if !body
                    .keys()
                    .all(|field| fields.contains_key(field) && field != tag_key)
                {
                    continue;
                }
                nested.extend(body.clone());
            }
            scalar => {
                let mut others = fields.keys().filter(|field| *field != tag_key);
                let (Some(field), None) = (others.next(), others.next()) else {
                    continue;
                };
                nested.insert(field.clone(), scalar.clone());
            }
        }
        let mut outer = object.clone();
        outer.insert(name.clone(), Value::Object(nested));
        return Some(Value::Object(outer));
    }
    None
}

/// A variant's fields flattened into the parent (see [`unflatten`]).
fn lift_flattened(value: &Value, schema: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let properties = schema.get("properties")?.as_object()?;
    let stray: Vec<&String> = object
        .keys()
        .filter(|key| !properties.contains_key(*key))
        .collect();
    if stray.is_empty() {
        return None;
    }
    for (name, property) in properties {
        let Some(tag) = object.get(name).and_then(Value::as_str) else {
            continue;
        };
        let variants = property
            .get("oneOf")
            .or_else(|| property.get("anyOf"))
            .and_then(Value::as_array);
        let Some(variants) = variants else {
            continue;
        };
        let mut matched = variants.iter().filter_map(|variant| {
            let fields = variant.get("properties")?.as_object()?;
            let (tag_key, _) = fields
                .iter()
                .find(|(_, field)| field.get("const").and_then(Value::as_str) == Some(tag))?;
            Some((tag_key.clone(), fields))
        });
        let (Some((tag_key, fields)), None) = (matched.next(), matched.next()) else {
            continue;
        };
        if !stray.iter().all(|key| fields.contains_key(*key)) {
            continue;
        }
        let mut nested = Map::new();
        nested.insert(tag_key, Value::String(tag.to_string()));
        let mut outer = Map::new();
        for (key, field) in object {
            if key == name {
                continue;
            }
            if properties.contains_key(key) {
                outer.insert(key.clone(), field.clone());
            } else {
                nested.insert(key.clone(), field.clone());
            }
        }
        outer.insert(name.clone(), Value::Object(nested));
        return Some(Value::Object(outer));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::unflatten;
    use serde_json::json;

    fn schema() -> serde_json::Value {
        json!({"type":"object","properties":{
            "message":{"type":"string"},
            "action":{"oneOf":[
                {"type":"object","properties":{"action":{"const":"write"},"file":{"type":"string"},"content":{"type":["string","null"]}}},
                {"type":"object","properties":{"action":{"const":"read"},"file":{"type":"string"}}}
            ]}
        }})
    }

    #[test]
    fn a_flattened_variant_is_folded_back_under_its_property() {
        let flat = json!({"message":"m","action":"write","file":"a.rs","content":"x"});
        assert_eq!(
            unflatten(&flat, &schema()),
            Some(json!({"message":"m","action":{"action":"write","file":"a.rs","content":"x"}}))
        );
    }

    #[test]
    fn a_variant_keyed_by_its_tag_is_rebuilt() {
        let schema = serde_json::json!({"type":"object","properties":{
            "message":{"type":"string"},
            "action":{"oneOf":[
                {"type":"object","properties":{"action":{"const":"replace"},"file":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}}},
                {"type":"object","properties":{"action":{"const":"apply_fix"},"fix":{"type":"integer"}}},
                {"type":"object","properties":{"action":{"const":"write"},"file":{"type":"string"},"content":{"type":["string","null"]}}}
            ]}
        }});
        let keyed = json!({"message":"m","action":{"replace":{"file":"a.rs","old_text":"x","new_text":"y"}}});
        assert_eq!(
            unflatten(&keyed, &schema),
            Some(
                json!({"message":"m","action":{"action":"replace","file":"a.rs","old_text":"x","new_text":"y"}})
            )
        );
        let scalar = json!({"message":"m","action":{"apply_fix":1}});
        assert_eq!(
            unflatten(&scalar, &schema),
            Some(json!({"message":"m","action":{"action":"apply_fix","fix":1}}))
        );
        // A scalar for a variant with two fields is ambiguous; a field the
        // variant lacks, or a tag key already present, is left alone.
        assert_eq!(
            unflatten(&json!({"message":"m","action":{"write":"x"}}), &schema),
            None
        );
        assert_eq!(
            unflatten(
                &json!({"message":"m","action":{"replace":{"file":"a","content":"x"}}}),
                &schema
            ),
            None
        );
        assert_eq!(
            unflatten(
                &json!({"message":"m","action":{"action":"replace"}}),
                &schema
            ),
            None
        );
    }

    #[test]
    fn nothing_is_guessed() {
        // Already nested: nothing to do.
        let nested = json!({"message":"m","action":{"action":"read","file":"a"}});
        assert_eq!(unflatten(&nested, &schema()), None);
        // A key the matched variant does not have.
        let unknown = json!({"message":"m","action":"read","file":"a","content":"x"});
        assert_eq!(unflatten(&unknown, &schema()), None);
        // A tag no variant carries.
        let untagged = json!({"message":"m","action":"delete","file":"a"});
        assert_eq!(unflatten(&untagged, &schema()), None);
    }
}
