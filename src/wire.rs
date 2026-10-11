use serde::{Deserialize, Serialize};
use serde_json::Value;

include!(concat!(env!("OUT_DIR"), "/wire.rs"));

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    Text(String),
}

#[derive(Debug, Deserialize)]
pub struct Envelope {
    pub id: Option<Id>,
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_generated_methods_and_both_ids() {
        let method: ServerMethod =
            serde_json::from_str("\"item/permissions/requestApproval\"").unwrap();
        assert_eq!(method, ServerMethod::ItemPermissionsRequestApproval);
        assert!(serde_json::from_str::<ServerMethod>("\"new/unknown\"").is_err());
        assert_ne!(Id::Text("1".into()), Id::Number(1));
    }
}
