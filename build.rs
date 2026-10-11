use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=protocol/contract.json");
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("protocol/contract.json").unwrap()).unwrap();
    let mut code = String::from(
        "// Generated from Codex JSON Schema. Do not edit.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]\npub enum ServerMethod {\n",
    );
    for method in schema["serverMethods"].as_array().unwrap() {
        let method = method.as_str().unwrap();
        let name = method
            .split('/')
            .map(|s| {
                let mut c = s.chars();
                format!("{}{}", c.next().unwrap().to_uppercase(), c.as_str())
            })
            .collect::<String>();
        code.push_str(&format!("#[serde(rename = {method:?})]\n{name},\n"));
    }
    code.push_str("}\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("wire.rs"),
        code,
    )
    .unwrap();
}
