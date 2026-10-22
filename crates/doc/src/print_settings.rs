//! Saved print settings: the slicer keys a design overrides on top of the printer, filament and
//! process files it is sliced with (OrcaSlicer key names, values as the files write them). A
//! design remembers how it prints; the files stay the user's profiles.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{DocError, Result};

pub const MAX_PRINT_KEYS: usize = 200;
pub const MAX_PRINT_KEY_BYTES: usize = 64;
pub const MAX_PRINT_VALUE_BYTES: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedSettings")]
pub struct PrintSettings {
    /// Slicer key to value; a value is a string, a number, a bool, or a list of those (a
    /// per-filament list), as OrcaSlicer files hold them.
    pub overlay: BTreeMap<String, Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UncheckedSettings {
    overlay: BTreeMap<String, Value>,
}

impl TryFrom<UncheckedSettings> for PrintSettings {
    type Error = DocError;
    fn try_from(value: UncheckedSettings) -> Result<Self> {
        let settings = Self { overlay: value.overlay };
        settings.validate()?;
        Ok(settings)
    }
}

impl PrintSettings {
    pub fn is_empty(&self) -> bool {
        self.overlay.is_empty()
    }

    /// A slicer key: lower-case letters, digits and underscores, at most 64 bytes.
    pub fn validate_key(key: &str) -> Result<()> {
        if key.is_empty() || key.len() > MAX_PRINT_KEY_BYTES || !key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(DocError::Invalid(format!(
                "`{key}` is not a slicer setting name (lower-case letters, digits and underscores, at most 64 bytes)"
            )));
        }
        Ok(())
    }

    /// A value the slicer can read: a finite number, a bool, a short string, or a list of those.
    pub fn validate_value(key: &str, value: &Value) -> Result<()> {
        let scalar = |v: &Value| match v {
            Value::String(s) => s.len() <= MAX_PRINT_VALUE_BYTES && !s.chars().any(char::is_control),
            Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
            Value::Bool(_) => true,
            _ => false,
        };
        let ok = match value {
            Value::Array(items) => items.len() <= 16 && items.iter().all(scalar),
            v => scalar(v),
        };
        if !ok {
            return Err(DocError::Invalid(format!("`{key}` must be a number, a bool, a short string, or a list of those")));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.overlay.len() > MAX_PRINT_KEYS {
            return Err(DocError::Invalid(format!("too many print settings (maximum {MAX_PRINT_KEYS})")));
        }
        for (key, value) in &self.overlay {
            Self::validate_key(key)?;
            Self::validate_value(key, value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_and_values_are_checked_on_the_way_in() {
        let ok: PrintSettings = serde_json::from_value(
            json!({"overlay": {"layer_height": "0.2", "wall_loops": 3, "enable_support": true, "nozzle_temperature": ["280"]}}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ok.overlay.len(), 4);
        for bad in [
            json!({"overlay": {"Layer Height": "0.2"}}),
            json!({"overlay": {"layer_height": {"x": 1}}}),
            json!({"overlay": {"layer_height": f64::NAN}}),
            json!({"overlay": {"layer_height": "x".repeat(300)}}),
            json!({"overlay": {}, "other": 1}),
        ] {
            assert!(serde_json::from_value::<PrintSettings>(bad.clone()).is_err(), "{bad}");
        }
        assert!(PrintSettings::default().is_empty());
    }
}
