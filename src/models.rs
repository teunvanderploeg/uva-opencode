use serde_json::{Value, json};

pub const CONFIG: &str = include_str!("../opencode.example.json");
pub const DEFAULT_MODEL: &str = "gpt-6-luna";

pub fn config() -> Value {
    serde_json::from_str(CONFIG).expect("embedded model configuration must be valid JSON")
}
pub fn catalog() -> Value {
    config()["providers"]["uva"]["models"].clone()
}
pub fn listing() -> Value {
    let catalog = catalog();
    let data: Vec<Value> = catalog
        .as_object()
        .expect("model catalog is an object")
        .iter()
        .filter(|(_, spec)| spec["disabled"] != true)
        .map(|(id, spec)| json!({"id":id,"name":spec["name"],"object":"model","owned_by":"uva"}))
        .collect();
    json!({"object":"list","data":data})
}
