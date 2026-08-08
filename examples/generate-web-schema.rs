use intagent::dashboard::DashboardSnapshot;
use intagent::run_detail::RunDetail;
use schemars::{JsonSchema, schema_for};
use serde_json::Value;

#[derive(JsonSchema)]
#[schemars(rename = "WebApiContract")]
pub struct WebApiContract {
    pub snapshot: DashboardSnapshot,
    pub run_detail: RunDetail,
}

// Dashboard responses serialize Option fields as required properties with null values.
fn require_object_properties(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                require_object_properties(value);
            }
        }
        Value::Object(object) => {
            let required = object
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| {
                    Value::Array(properties.keys().cloned().map(Value::String).collect())
                });
            for value in object.values_mut() {
                require_object_properties(value);
            }
            if let Some(required) = required {
                object.insert("required".into(), required);
            }
        }
        _ => {}
    }
}

fn main() {
    let mut schema = serde_json::to_value(schema_for!(WebApiContract)).unwrap();
    require_object_properties(&mut schema);
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}
