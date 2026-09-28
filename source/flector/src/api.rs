use kube::CustomResource;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const FIELD_MANAGER: &str = "flector";
pub const FINALIZER: &str = "flector.io/cleanup";
pub const SOURCE_ANNOTATION: &str = "flector.io/source";
pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
pub const MANAGED_BY: &str = "flector";

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(group = "flector.io", version = "v1alpha1", kind = "Flect", namespaced, status = "FlectStatus", doc = "")]
#[serde(rename_all = "camelCase")]
pub struct FlectSpec {
    pub source: SourceRef,
    pub destinations: Vec<Destination>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub kind: SourceKind,
    pub namespace: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
pub enum SourceKind {
    ConfigMap,
    Secret,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
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

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlectStatus {
    pub synced: Option<bool>,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    #[test]
    fn crd_contract() {
        let crd = Flect::crd();
        assert_eq!(crd.metadata.name.as_deref(), Some("flects.flector.io"));
        assert_eq!(crd.spec.group, "flector.io");
        assert_eq!(crd.spec.names.kind, "Flect");
        assert!(crd.spec.names.short_names.is_none());
        assert_eq!(crd.spec.versions[0].name, "v1alpha1");
        assert!(crd.spec.versions[0].subresources.is_some());
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let root = &schema["openAPIV3Schema"];
        assert!(root.is_object());
        assert!(root.get("description").is_none());
    }

    #[test]
    fn destination_name_defaults_to_source() {
        let destination = Destination { namespace: "b".into(), name: None };
        assert_eq!(destination.name("app-config"), "app-config");
        let destination = Destination { namespace: "b".into(), name: Some("renamed".into()) };
        assert_eq!(destination.name("app-config"), "renamed");
    }
}
