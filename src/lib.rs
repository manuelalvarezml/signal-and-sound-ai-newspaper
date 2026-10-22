pub mod aws;
pub mod collect;
pub mod compose;
pub mod interests;
pub mod model;
pub mod pipeline;
pub mod render;
pub mod search;
pub mod state;

pub fn config(name: &str) -> anyhow::Result<serde_json::Value> {
    let base = std::env::var("APP_ROOT").unwrap_or_else(|_| ".".into());
    let path = std::path::Path::new(&base)
        .join("config")
        .join(format!("{name}.json"));
    let path = if path.exists() {
        path
    } else {
        path.with_file_name(format!("{name}.example.json"))
    };
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
pub fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
