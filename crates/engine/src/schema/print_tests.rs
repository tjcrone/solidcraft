use std::sync::Arc;

use serde_json::json;

use super::{StartParams, StartResult};
use crate::schema::tests::{assert_keys, conforms};
use crate::schema::{ParamCheck, input_schema_for, output_schema_for};
use crate::tests::print_host::{Fake, sliced_session};

#[test]
fn schemas_and_the_key_check_agree() {
    assert_keys(input_schema_for::<StartParams>(), &["name"]);
}

#[test]
fn print_start_replies_match_the_output_schema_and_start_the_upload() {
    let schema = output_schema_for::<StartResult>();
    let schema = schema.as_value();
    let host = Arc::new(Fake::default());
    let mut s = sliced_session(&host);
    s.param_check = ParamCheck::Warn;
    for params in [json!({}), json!({"name": "clamp block"}), json!({"name": null})] {
        let r = s.execute("print.start", &params).unwrap_or_else(|e| panic!("{params}: {e}"));
        assert!(conforms(schema, schema, &r), "{r} doesn't match {schema}");
        assert_eq!(r.get("param_warnings"), None, "{r}");
        assert_eq!(r["start"], json!(true), "{r}");
    }
    let uploads = host.uploads.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(uploads.iter().map(|(name, _, start)| (name.as_str(), *start)).collect::<Vec<_>>(), [("Untitled.gcode", true), ("clamp block.gcode", true), ("Untitled.gcode", true)]);
    assert!(!conforms(schema, schema, &json!({"job": 1, "name": "a.gcode", "bytes": 3})));
}
