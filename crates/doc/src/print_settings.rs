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

/// Where the design sits on the printer's bed: millimetres from the bed's centre, and a turn
/// about Z in degrees. The default is the middle of the bed, as the design is modelled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
}

/// Farthest from the bed's centre a placement may ask for, mm (no bed is that big).
pub const MAX_PLACEMENT_MM: f64 = 5000.0;

impl Placement {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        for (name, v) in [("x", self.x), ("y", self.y)] {
            if !v.is_finite() || v.abs() > MAX_PLACEMENT_MM {
                return Err(DocError::Invalid(format!("placement {name} must be a finite number within {MAX_PLACEMENT_MM} mm")));
            }
        }
        if !self.rotation.is_finite() || self.rotation.abs() > 360.0 {
            return Err(DocError::Invalid("placement rotation must be a finite number of degrees within a turn".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedSettings")]
pub struct PrintSettings {
    /// Slicer key to value; a value is a string, a number, a bool, or a list of those (a
    /// per-filament list), as OrcaSlicer files hold them.
    pub overlay: BTreeMap<String, Value>,
    /// Where the design sits on the bed; left out of the file when it is the bed's centre.
    #[serde(default, skip_serializing_if = "Placement::is_default")]
    pub placement: Placement,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UncheckedSettings {
    overlay: BTreeMap<String, Value>,
    placement: Placement,
}

impl TryFrom<UncheckedSettings> for PrintSettings {
    type Error = DocError;
    fn try_from(value: UncheckedSettings) -> Result<Self> {
        let settings = Self { overlay: value.overlay, placement: value.placement };
        settings.validate()?;
        Ok(settings)
    }
}

impl PrintSettings {
    /// No overrides and the default placement: nothing to save.
    pub fn is_empty(&self) -> bool {
        self.overlay.is_empty() && self.placement.is_default()
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
        self.placement.validate()
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

    #[test]
    fn a_placement_is_saved_only_off_centre_and_checked_on_the_way_in() {
        let centred = PrintSettings::default();
        assert!(!serde_json::to_string(&centred).unwrap_or_default().contains("placement"));
        let moved: PrintSettings =
            serde_json::from_value(json!({"overlay": {}, "placement": {"x": 12.5, "y": -3.0, "rotation": 90.0}})).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(moved.placement, Placement { x: 12.5, y: -3.0, rotation: 90.0 });
        assert!(!moved.is_empty());
        for bad in [
            json!({"placement": {"x": 1e9}}),
            json!({"placement": {"x": f64::NAN}}),
            json!({"placement": {"rotation": 400.0}}),
            json!({"placement": {"z": 1.0}}),
        ] {
            assert!(serde_json::from_value::<PrintSettings>(bad.clone()).is_err(), "{bad}");
        }
    }
}
