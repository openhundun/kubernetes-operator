use crate::api::FIELD_MANAGER;
use crate::api::FINALIZER;
use crate::api::Flect;
use crate::api::FlectStatus;
use crate::api::MANAGED_BY;
use crate::api::MANAGED_BY_LABEL;
use crate::api::SOURCE_ANNOTATION;
use crate::api::SourceKind;
use crate::api::SourceRef;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use kube::Client;
use kube::Resource;
use kube::ResourceExt;
use kube::api::ApiResource;
use kube::api::DeleteParams;
use kube::api::DynamicObject;
use kube::api::GroupVersionKind;
use kube::api::Patch;
use kube::api::PatchParams;
use kube::runtime::controller::Action;
use kube::runtime::events::Event;
use kube::runtime::events::EventType;
use kube::runtime::events::Recorder;
use kube::runtime::reflector::ObjectRef;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

pub const REQUEUE: Duration = Duration::from_secs(300);
pub const RETRY: Duration = Duration::from_secs(30);

pub struct Ctx {
    pub client: Client,
    pub recorder: Recorder,
}

pub enum Source {
    ConfigMap(ConfigMap),
    Secret(Secret),
}

pub enum Step {
    Apply { namespace: String, name: String, body: Value },
    Delete { namespace: String, name: String },
}

pub struct Outcome {
    action: &'static str,
    done: usize,
    total: usize,
}

impl Outcome {
    pub fn synced(&self) -> bool {
        self.done == self.total
    }

    pub fn message(&self) -> String {
        format!("{} {}/{}", self.action, self.done, self.total)
    }
}

impl Source {
    pub fn payload(&self) -> Value {
        match self {
            Source::ConfigMap(config_map) => json!({
                "kind": "ConfigMap",
                "data": config_map.data.clone(),
                "binaryData": config_map.binary_data.clone(),
                "immutable": config_map.immutable,
            }),
            Source::Secret(secret) => json!({
                "kind": "Secret",
                "data": secret.data.clone(),
                "type": secret.type_.clone(),
                "immutable": secret.immutable,
            }),
        }
    }
}

pub fn copy_body(payload: &Value, namespace: &str, name: &str, source: &str) -> Value {
    let mut body = payload.clone();
    body["apiVersion"] = json!("v1");
    body["metadata"] = json!({
        "name": name,
        "namespace": namespace,
        "labels": { MANAGED_BY_LABEL: MANAGED_BY },
        "annotations": { SOURCE_ANNOTATION: source },
    });
    body
}

pub fn plan(flect: &Flect, source: Option<&Source>) -> Vec<Step> {
    let payload = source.map(Source::payload);
    let source_ref = format!("{}/{}", flect.spec.source.namespace, flect.spec.source.name);
    flect
        .spec
        .destinations
        .iter()
        .map(|destination| {
            let name = destination.name(&flect.spec.source.name).to_string();
            match &payload {
                Some(payload) => Step::Apply {
                    namespace: destination.namespace.clone(),
                    name: name.clone(),
                    body: copy_body(payload, &destination.namespace, &name, &source_ref),
                },
                None => Step::Delete { namespace: destination.namespace.clone(), name },
            }
        })
        .collect()
}

pub fn affected(flects: &[Arc<Flect>], kind: SourceKind, namespace: &str, name: &str, copy_of: Option<&str>) -> Vec<ObjectRef<Flect>> {
    let hit = |flect: &Arc<Flect>, namespace: &str, name: &str| flect.spec.source.kind == kind && flect.spec.source.namespace == namespace && flect.spec.source.name == name;
    let mut refs = flects.iter().filter(|flect| hit(flect, namespace, name)).map(|flect| ObjectRef::from_obj(&**flect)).collect::<Vec<_>>();
    if let Some((namespace, name)) = copy_of.and_then(|value| value.split_once('/')) {
        refs.extend(flects.iter().filter(|flect| hit(flect, namespace, name)).map(|flect| ObjectRef::from_obj(&**flect)));
    }
    refs
}

pub async fn fetch(client: &Client, source: &SourceRef) -> Result<Option<Source>, kube::Error> {
    Ok(match source.kind {
        SourceKind::ConfigMap => Api::<ConfigMap>::namespaced(client.clone(), &source.namespace).get_opt(&source.name).await?.map(Source::ConfigMap),
        SourceKind::Secret => Api::<Secret>::namespaced(client.clone(), &source.namespace).get_opt(&source.name).await?.map(Source::Secret),
    })
}

pub async fn reconcile(flect: Arc<Flect>, ctx: Arc<Ctx>) -> Result<Action, kube::Error> {
    let api = Api::<Flect>::namespaced(ctx.client.clone(), &flect.namespace().unwrap_or_default());
    let name = flect.name_any();

    if flect.metadata.deletion_timestamp.is_some() {
        run(&ctx, &flect, None).await;
        if flect.finalizers().iter().any(|finalizer| finalizer == FINALIZER) {
            let finalizers = flect.finalizers().iter().filter(|finalizer| *finalizer != FINALIZER).cloned().collect::<Vec<_>>();
            api.patch(&name, &PatchParams::default(), &Patch::Merge(json!({ "metadata": { "finalizers": finalizers } }))).await?;
        }
        return Ok(Action::await_change());
    }

    if !flect.finalizers().iter().any(|finalizer| finalizer == FINALIZER) {
        let mut finalizers = flect.finalizers().to_vec();
        finalizers.push(FINALIZER.to_string());
        api.patch(&name, &PatchParams::default(), &Patch::Merge(json!({ "metadata": { "finalizers": finalizers } }))).await?;
    }

    let source = fetch(&ctx.client, &flect.spec.source).await?;
    let outcome = run(&ctx, &flect, source.as_ref()).await;
    let changed = set_status(&api, &flect, &outcome).await?;
    if changed || !outcome.synced() {
        let (type_, reason) = if outcome.synced() { (EventType::Normal, "Synced") } else { (EventType::Warning, "SyncFailed") };
        record(&ctx, &flect, type_, reason, &outcome.message()).await;
    }
    Ok(Action::requeue(REQUEUE))
}

pub fn error_policy(flect: Arc<Flect>, error: &kube::Error, _ctx: Arc<Ctx>) -> Action {
    if matches!(error, kube::Error::Api(status) if status.is_not_found()) {
        return Action::await_change();
    }
    warn!(%error, flect = %flect.name_any(), "reconcile failed");
    Action::requeue(RETRY)
}

async fn run(ctx: &Ctx, flect: &Flect, source: Option<&Source>) -> Outcome {
    let kind = flect.spec.source.kind;
    let steps = plan(flect, source);
    let mut done = 0;
    for step in &steps {
        if execute(&ctx.client, kind, step).await {
            done += 1;
        }
    }
    Outcome { action: if source.is_some() { "applied" } else { "deleted" }, done, total: steps.len() }
}

async fn execute(client: &Client, kind: SourceKind, step: &Step) -> bool {
    match step {
        Step::Apply { namespace, name, body } => match copies(client, kind, namespace).patch(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(body)).await {
            Ok(_) => true,
            Err(error) => {
                warn!(%error, %namespace, %name, "applying copy failed");
                false
            }
        },
        Step::Delete { namespace, name } => match copies(client, kind, namespace).delete(name, &DeleteParams::default()).await {
            Ok(_) => true,
            Err(kube::Error::Api(status)) if status.is_not_found() => true,
            Err(error) => {
                warn!(%error, %namespace, %name, "deleting copy failed");
                false
            }
        },
    }
}

async fn set_status(api: &Api<Flect>, flect: &Flect, outcome: &Outcome) -> Result<bool, kube::Error> {
    let status = FlectStatus { synced: Some(outcome.synced()), message: Some(outcome.message()) };
    if flect.status.as_ref() == Some(&status) {
        return Ok(false);
    }
    api.patch_status(&flect.name_any(), &PatchParams::default(), &Patch::Merge(json!({ "status": status }))).await?;
    Ok(true)
}

async fn record(ctx: &Ctx, flect: &Flect, type_: EventType, reason: &str, note: &str) {
    let event = Event {
        type_,
        reason: reason.to_string(),
        note: Some(note.to_string()),
        action: "Sync".to_string(),
        secondary: None,
    };
    if let Err(error) = ctx.recorder.publish(&event, &flect.object_ref(&())).await {
        warn!(%error, "publishing event failed");
    }
}

fn copies(client: &Client, kind: SourceKind, namespace: &str) -> Api<DynamicObject> {
    Api::namespaced_with(
        client.clone(),
        namespace,
        &ApiResource::from_gvk(&GroupVersionKind::gvk(
            "",
            "v1",
            match kind {
                SourceKind::ConfigMap => "ConfigMap",
                SourceKind::Secret => "Secret",
            },
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Destination;
    use crate::api::FlectSpec;
    use k8s_openapi::ByteString;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    fn flect(namespace: &str, destinations: &[&str], name: Option<&str>) -> Flect {
        let mut flect = Flect::new(
            "shared",
            FlectSpec {
                source: SourceRef { kind: SourceKind::ConfigMap, namespace: "a".into(), name: "shared".into() },
                destinations: destinations
                    .iter()
                    .map(|target| Destination { namespace: target.to_string(), name: name.map(str::to_string) })
                    .collect(),
            },
        );
        flect.metadata.namespace = Some(namespace.into());
        flect
    }

    fn config_map() -> ConfigMap {
        ConfigMap {
            metadata: ObjectMeta { name: Some("shared".into()), namespace: Some("a".into()), ..Default::default() },
            data: Some(BTreeMap::from([("k".to_string(), "v".to_string())])),
            binary_data: Some(BTreeMap::from([("b".to_string(), ByteString(vec![1, 2, 3]))])),
            ..Default::default()
        }
    }

    #[test]
    fn payload_keeps_kind_and_data() {
        let payload = Source::ConfigMap(config_map()).payload();
        assert_eq!(payload["kind"], "ConfigMap");
        assert_eq!(payload["data"]["k"], "v");
        assert_eq!(payload["binaryData"]["b"], "AQID");
        assert_eq!(payload["immutable"], Value::Null);
    }

    #[test]
    fn secret_payload_keeps_type_and_base64_data() {
        let secret = Secret {
            metadata: ObjectMeta { name: Some("shared".into()), namespace: Some("a".into()), ..Default::default() },
            type_: Some("kubernetes.io/tls".into()),
            data: Some(BTreeMap::from([("tls.crt".to_string(), ByteString(b"crt".to_vec()))])),
            ..Default::default()
        };
        let payload = Source::Secret(secret).payload();
        assert_eq!(payload["kind"], "Secret");
        assert_eq!(payload["type"], "kubernetes.io/tls");
        assert_eq!(payload["data"]["tls.crt"], "Y3J0");
    }

    #[test]
    fn copy_body_marks_and_relocates_the_copy() {
        let body = copy_body(&Source::ConfigMap(config_map()).payload(), "b", "renamed", "a/shared");
        assert_eq!(body["apiVersion"], "v1");
        assert_eq!(body["metadata"]["name"], "renamed");
        assert_eq!(body["metadata"]["namespace"], "b");
        assert_eq!(body["metadata"]["labels"][MANAGED_BY_LABEL], MANAGED_BY);
        assert_eq!(body["metadata"]["annotations"][SOURCE_ANNOTATION], "a/shared");
        assert_eq!(body["data"]["k"], "v");
    }

    #[test]
    fn plan_applies_to_each_destination_with_default_name() {
        let source = Source::ConfigMap(config_map());
        let steps = plan(&flect("x", &["b", "c"], None), Some(&source));
        assert_eq!(steps.len(), 2);
        for (index, namespace) in ["b", "c"].into_iter().enumerate() {
            match &steps[index] {
                Step::Apply { namespace: step_namespace, name, body } => {
                    assert_eq!(step_namespace, namespace);
                    assert_eq!(name, "shared");
                    assert_eq!(body["metadata"]["namespace"], namespace);
                    assert_eq!(body["metadata"]["annotations"][SOURCE_ANNOTATION], "a/shared");
                }
                Step::Delete { .. } => panic!("expected apply step"),
            }
        }
    }

    #[test]
    fn plan_honours_destination_name_override() {
        let steps = plan(&flect("x", &["b"], Some("copy-of-shared")), Some(&Source::ConfigMap(config_map())));
        match &steps[0] {
            Step::Apply { name, .. } => assert_eq!(name, "copy-of-shared"),
            Step::Delete { .. } => panic!("expected apply step"),
        }
    }

    #[test]
    fn plan_deletes_copies_without_source() {
        let steps = plan(&flect("x", &["b"], None), None);
        match &steps[0] {
            Step::Delete { namespace, name } => {
                assert_eq!(namespace, "b");
                assert_eq!(name, "shared");
            }
            Step::Apply { .. } => panic!("expected delete step"),
        }
    }

    #[test]
    fn affected_selects_referencing_flects() {
        let flects = vec![Arc::new(flect("x", &["b"], None))];
        assert_eq!(affected(&flects, SourceKind::ConfigMap, "a", "shared", None).len(), 1);
        assert_eq!(affected(&flects, SourceKind::ConfigMap, "a", "shared", None)[0].name, "shared");
        assert!(affected(&flects, SourceKind::Secret, "a", "shared", None).is_empty());
        assert!(affected(&flects, SourceKind::ConfigMap, "a", "other", None).is_empty());
    }

    #[test]
    fn affected_follows_copy_annotation_back_to_its_source() {
        let flects = vec![Arc::new(flect("x", &["b"], None))];
        let from_copy = affected(&flects, SourceKind::ConfigMap, "b", "shared", Some("a/shared"));
        assert_eq!(from_copy.len(), 1);
        assert!(affected(&flects, SourceKind::ConfigMap, "b", "shared", Some("other/thing")).is_empty());
    }

    #[test]
    fn outcome_reports_progress() {
        let outcome = Outcome { action: "applied", done: 1, total: 2 };
        assert!(!outcome.synced());
        assert_eq!(outcome.message(), "applied 1/2");
        assert!(Outcome { action: "deleted", done: 0, total: 0 }.synced());
    }
}
