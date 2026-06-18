use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}
