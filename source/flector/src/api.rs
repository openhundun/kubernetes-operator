pub const FIELD_MANAGER: &str = "flector";
pub const FINALIZER: &str = "flector.io/cleanup";
pub const SOURCE_ANNOTATION: &str = "flector.io/source";
pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
pub const MANAGED_BY: &str = "flector";

#[derive(Clone, Debug, kube::CustomResource, schemars::JsonSchema, serde::Deserialize, serde::Serialize)]
#[kube(doc = "", group = "flector.io", kind = "Flect", namespaced, status = "FlectStatus", version = "v1alpha1")]
#[serde(rename_all = "camelCase")]
pub struct FlectSpec {
    pub source: SourceRef,
    pub destinations: Vec<Destination>,
}

#[derive(Clone, Debug, schemars::JsonSchema, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub kind: SourceKind,
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, schemars::JsonSchema, serde::Deserialize, serde::Serialize)]
pub enum SourceKind {
    ConfigMap,
    Secret,
}

#[derive(Clone, Debug, schemars::JsonSchema, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Destination {
    pub fn name<'a>(&'a self, source: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(source)
    }
}

#[derive(Clone, Debug, Default, PartialEq, schemars::JsonSchema, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlectStatus {
    pub synced: Option<bool>,
    pub message: Option<String>,
}
