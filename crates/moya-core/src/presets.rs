//! Preset JSON interoperability: Moya and Oniric_Presets.json / userPresets.json
//! store `[[s0..s46, "name"], ...]`; legacy arrays contain 45 or 46 numbers.
use crate::Params;
use serde::ser::{Serialize, SerializeSeq, Serializer};

#[derive(Clone, Debug, PartialEq)]
pub struct Preset { pub name: String, pub params: Params }

#[derive(Debug, thiserror::Error)]
pub enum PresetError { #[error("invalid preset json: {0}")] Json(#[from] serde_json::Error), #[error("{0}")] Shape(String) }

/// Read current and legacy presets in stored order; non-numeric slots use loader defaults.
pub fn parse_presets(json: &str) -> Result<Vec<Preset>, PresetError> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let entries = value.as_array().ok_or_else(|| {
        PresetError::Shape("presets must be an array".into())
    })?;
    entries.iter().enumerate().map(|(index, entry)| {
        let array = entry.as_array().ok_or_else(|| {
            PresetError::Shape(format!("preset {index} must be an array"))
        })?;
        if !(46..=48).contains(&array.len()) {
            return Err(PresetError::Shape(format!(
                "preset {index} must contain 45, 46 or 47 slots followed by a name"
            )));
        }
        let name = array.last().and_then(serde_json::Value::as_str).ok_or_else(|| {
            PresetError::Shape(format!("preset {index} must end with a string name"))
        })?;
        let mut slots = [None; 47];
        for (slot, value) in slots.iter_mut().zip(&array[..array.len() - 1]) {
            *slot = if value.is_null() { None } else { Some(value.as_f64().unwrap_or(f64::NAN)) };
        }
        Ok(Preset {
            name: name.to_owned(),
            params: Params::from_slots(&slots[..array.len() - 1]),
        })
    }).collect()
}

pub fn serialize_presets(presets: &[Preset]) -> String {
    struct PresetArray<'a>(&'a Preset);

    impl Serialize for PresetArray<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut sequence = serializer.serialize_seq(Some(48))?;
            for slot in self.0.params.to_slots() {
                sequence.serialize_element(&slot)?;
            }
            sequence.serialize_element(&self.0.name)?;
            sequence.end()
        }
    }

    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut bytes);
    serializer.collect_seq(presets.iter().map(PresetArray))
        .expect("serializing preset slots and names to memory cannot fail");
    String::from_utf8(bytes).expect("JSON serialization produces UTF-8")
}
/// Glow-layer name metadata string: 47 values comma-separated, no brackets.
pub fn params_to_layer_name(p: &Params) -> String {
    use std::fmt::Write;

    let mut name = String::new();
    for (index, slot) in p.to_slots().iter().enumerate() {
        if index != 0 {
            name.push(',');
        }
        write!(name, "{slot}").expect("writing to a String cannot fail");
    }
    name
}

/// Read numeric metadata with optional brackets, including legacy 45/46-slot names.
pub fn params_from_layer_name(s: &str) -> Result<Params, PresetError> {
    let s = s.trim();
    let values: Vec<f64> = if s.starts_with('[') {
        serde_json::from_str(s)?
    } else {
        serde_json::from_str(&format!("[{s}]"))?
    };
    if !(45..=47).contains(&values.len()) {
        return Err(PresetError::Shape("layer metadata must contain 45, 46 or 47 numbers".into()));
    }
    let count = values.len();
    let mut slots = [None; 47];
    for (slot, value) in slots.iter_mut().zip(values) {
        *slot = Some(value);
    }
    Ok(Params::from_slots(&slots[..count]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const DEFAULT_JSON: &str = r#"[[0.15,30,100,100,1000,1,1.3,1.5,100,0,0,0,0,0,4,4,100,2,0,0,0,0,0,0,0,0,0,0,0,45,0,0,1,0,0,0,0,0,0.5,0.5,5,20,0.5,0,0,1,11,"My Preset"]]"#;

    #[test]
    fn default_json_example_and_serialization() {
        let presets = parse_presets(DEFAULT_JSON).unwrap();
        assert_eq!(presets, vec![Preset {
            name: "My Preset".into(),
            params: Params::default(),
        }]);
        let serialized = serialize_presets(&presets);
        assert_eq!(parse_presets(&serialized).unwrap(), presets);
        let values: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(values[0].as_array().unwrap().len(), 48);
        assert!(values[0].as_array().unwrap()[..47].iter().all(Value::is_number));
        assert_eq!(serialize_presets(&[]), "[]");
        assert!(parse_presets("[]").unwrap().is_empty());
    }

    #[test]
    fn legacy_arrays_default_missing_slots_and_keep_order() {
        let mut entries = Vec::new();
        for count in [45, 46] {
            let mut slots = Params::default().to_slots().map(Value::from).to_vec();
            slots[45] = json!(2.5);
            slots.truncate(count);
            slots.push(json!(format!("Legacy {count}")));
            entries.push(Value::Array(slots));
        }
        let presets = parse_presets(&Value::Array(entries).to_string()).unwrap();
        assert_eq!(presets[0].name, "Legacy 45");
        assert_eq!(presets[1].name, "Legacy 46");
        assert_eq!(presets[0].params.glare_gamma, 1.0);
        assert_eq!(presets[1].params.glare_gamma, 2.5);
        assert_eq!(presets[0].params.glow_iterations, 11.0);
        assert_eq!(presets[1].params.glow_iterations, 11.0);
        assert_eq!(parse_presets(&serialize_presets(&presets)).unwrap(), presets);
    }

    #[test]
    fn malformed_json_and_preset_shapes_are_rejected() {
        assert!(matches!(parse_presets("["), Err(PresetError::Json(_))));
        for input in ["null", "{}", "1", r#""text""#, "[null]", "[{}]", "[[]]"] {
            assert!(matches!(parse_presets(input), Err(PresetError::Shape(_))), "{input}");
        }
        for count in [0, 44, 48] {
            let mut entry = vec![json!(0); count];
            entry.push(json!("Invalid"));
            assert!(matches!(
                parse_presets(&json!([entry]).to_string()),
                Err(PresetError::Shape(_))
            ));
        }
        for name in [Value::Null, json!(123), json!(true), json!([]), json!({})] {
            let mut entry = Params::default().to_slots().map(Value::from).to_vec();
            entry.push(name);
            assert!(matches!(
                parse_presets(&json!([entry]).to_string()),
                Err(PresetError::Shape(_))
            ));
        }
    }

    #[test]
    fn non_number_slider_values_use_defaults() {
        for invalid in [Value::Null, json!("0.75"), json!(false), json!([]), json!({})] {
            let mut entry = Params::default().to_slots().map(Value::from).to_vec();
            for slot in [0, 1, 6, 45, 46] {
                entry[slot] = invalid.clone();
            }
            entry.push(json!("Fallback"));
            let presets = parse_presets(&json!([entry]).to_string()).unwrap();
            assert_eq!(presets[0].params, Params::default());
        }
    }

    #[test]
    fn null_loader_fallback_differs_from_invalid_slider_default() {
        for (value, expected_direction) in [(json!("invalid"), 45.0), (Value::Null, -35.0)] {
            let mut entry = Params::default().to_slots().map(Value::from).to_vec();
            entry[29] = value;
            entry[34] = Value::Null;
            entry.push(json!("Loader fallback"));
            let presets = parse_presets(&json!([entry]).to_string()).unwrap();
            assert_eq!(presets[0].params.ca_direction, expected_direction);
            assert!(presets[0].params.ca_mix);
        }
    }

    #[test]
    fn metadata_roundtrip_and_validation() {
        let params = Params {
            threshold: 0.37,
            exposure: -2.25,
            ca_distance: 12.75,
            glare_gamma: 2.5,
            glow_iterations: 6.0,
            ..Params::default()
        };
        let metadata = params_to_layer_name(&params);
        assert!(!metadata.contains(['[', ']']));
        assert_eq!(metadata.split(',').count(), 47);
        let expected = Params::from_slots(&params.to_slots().map(Some));
        for text in [metadata.clone(), format!(" \n[{metadata}] \t")] {
            assert_eq!(params_from_layer_name(&text).unwrap(), expected);
        }
        for count in [45, 46] {
            let legacy = metadata.split(',').take(count).collect::<Vec<_>>().join(",");
            let loaded = params_from_layer_name(&legacy).unwrap();
            assert_eq!(loaded.glow_iterations, 11.0);
            assert_eq!(loaded.glare_gamma, if count == 45 { 1.0 } else { 2.5 });
        }
        for invalid in ["", "[]", "1,2", "[1,2", "NaN", "null", "{}"] {
            assert!(params_from_layer_name(invalid).is_err(), "{invalid}");
        }
        for invalid in ["null", r#""1""#, "true", "[]", "{}"] {
            let malformed = metadata.replacen("0.37", invalid, 1);
            assert!(params_from_layer_name(&malformed).is_err());
        }
    }
}
