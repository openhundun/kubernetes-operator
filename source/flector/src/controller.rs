pub const REQUEUE: std::time::Duration = std::time::Duration::from_secs(300);
pub const RETRY: std::time::Duration = std::time::Duration::from_secs(30);

pub struct Ctx {
    pub client: kube::Client,
    pub recorder: kube::runtime::events::Recorder,
}

pub enum Source {
    ConfigMap(k8s_openapi::api::core::v1::ConfigMap),
    Secret(k8s_openapi::api::core::v1::Secret),
}

pub enum Step {
    Apply { namespace: String, name: String, body: serde_json::Value },
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
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Source::ConfigMap(config_map) => serde_json::json!({
                "kind": "ConfigMap",
                "data": config_map.data.clone(),
                "binaryData": config_map.binary_data.clone(),
                "immutable": config_map.immutable,
            }),
            Source::Secret(secret) => serde_json::json!({
                "kind": "Secret",
                "data": secret.data.clone(),
                "type": secret.type_.clone(),
                "immutable": secret.immutable,
            }),
        }
    }
}

pub fn copy_body(payload: &serde_json::Value, namespace: &str, name: &str, source: &str) -> serde_json::Value {
    let mut body = payload.clone();
    body["apiVersion"] = serde_json::json!("v1");
    body["metadata"] = serde_json::json!({
        "name": name,
        "namespace": namespace,
        "labels": { crate::api::MANAGED_BY_LABEL: crate::api::MANAGED_BY },
        "annotations": { crate::api::SOURCE_ANNOTATION: source },
    });
    body
}

pub fn plan(flect: &crate::api::Flect, source: Option<&Source>) -> Vec<Step> {
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

pub fn affected(
    flects: &[std::sync::Arc<crate::api::Flect>],
    kind: crate::api::SourceKind,
    namespace: &str,
    name: &str,
    copy_of: Option<&str>,
) -> Vec<kube::runtime::reflector::ObjectRef<crate::api::Flect>> {
    let hit = |flect: &std::sync::Arc<crate::api::Flect>, namespace: &str, name: &str| flect.spec.source.kind == kind && flect.spec.source.namespace == namespace && flect.spec.source.name == name;
    let mut refs = flects
        .iter()
        .filter(|flect| hit(flect, namespace, name))
        .map(|flect| kube::runtime::reflector::ObjectRef::from_obj(&**flect))
        .collect::<Vec<_>>();
    if let Some((namespace, name)) = copy_of.and_then(|value| value.split_once('/')) {
        refs.extend(
            flects
                .iter()
                .filter(|flect| hit(flect, namespace, name))
                .map(|flect| kube::runtime::reflector::ObjectRef::from_obj(&**flect)),
        );
    }
    refs
}

pub async fn fetch(client: &kube::Client, source: &crate::api::SourceRef) -> Result<Option<Source>, kube::Error> {
    Ok(match source.kind {
        crate::api::SourceKind::ConfigMap => kube::Api::<k8s_openapi::api::core::v1::ConfigMap>::namespaced(client.clone(), &source.namespace)
            .get_opt(&source.name)
            .await?
            .map(Source::ConfigMap),
        crate::api::SourceKind::Secret => kube::Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), &source.namespace)
            .get_opt(&source.name)
            .await?
            .map(Source::Secret),
    })
}

pub async fn reconcile(flect: std::sync::Arc<crate::api::Flect>, ctx: std::sync::Arc<Ctx>) -> Result<kube::runtime::controller::Action, kube::runtime::finalizer::Error<kube::Error>> {
    use kube::ResourceExt;
    let api = kube::Api::<crate::api::Flect>::namespaced(ctx.client.clone(), &flect.namespace().unwrap_or_default());
    kube::runtime::finalizer::finalizer(&api, crate::api::FINALIZER, flect, {
        let api = api.clone();
        move |event| async move {
            match event {
                kube::runtime::finalizer::Event::Apply(flect) => {
                    let source = fetch(&ctx.client, &flect.spec.source).await?;
                    let (outcome, _) = run(&ctx, &flect, source.as_ref()).await;
                    let changed = set_status(&api, &flect, &outcome).await?;
                    if changed || !outcome.synced() {
                        let (type_, reason) = if outcome.synced() {
                            (kube::runtime::events::EventType::Normal, "Synced")
                        } else {
                            (kube::runtime::events::EventType::Warning, "SyncFailed")
                        };
                        record(&ctx, &flect, type_, reason, &outcome.message()).await;
                    }
                    Ok(kube::runtime::controller::Action::requeue(if outcome.synced() { REQUEUE } else { RETRY }))
                }
                kube::runtime::finalizer::Event::Cleanup(flect) => match run(&ctx, &flect, None).await.1 {
                    None => Ok(kube::runtime::controller::Action::await_change()),
                    Some(error) => Err(error),
                },
            }
        }
    })
    .await
}

pub fn error_policy(flect: std::sync::Arc<crate::api::Flect>, error: &kube::runtime::finalizer::Error<kube::Error>, _ctx: std::sync::Arc<Ctx>) -> kube::runtime::controller::Action {
    use kube::ResourceExt;
    if matches!(error, kube::runtime::finalizer::Error::ApplyFailed(kube::Error::Api(status)) if status.is_not_found()) {
        return kube::runtime::controller::Action::await_change();
    }
    tracing::warn!(%error, flect = %flect.name_any(), "reconcile failed");
    kube::runtime::controller::Action::requeue(RETRY)
}

async fn run(ctx: &Ctx, flect: &crate::api::Flect, source: Option<&Source>) -> (Outcome, Option<kube::Error>) {
    let kind = flect.spec.source.kind;
    let steps = plan(flect, source);
    let mut done = 0;
    let mut failure = None;
    for step in &steps {
        match execute(&ctx.client, kind, step).await {
            Ok(()) => done += 1,
            Err(error) => failure = failure.or(Some(error)),
        }
    }
    let action = if source.is_some() { "applied" } else { "deleted" };
    (Outcome { action, done, total: steps.len() }, failure)
}

async fn execute(client: &kube::Client, kind: crate::api::SourceKind, step: &Step) -> Result<(), kube::Error> {
    match step {
        Step::Apply { namespace, name, body } => {
            match copies(client, kind, namespace)
                .patch(name, &kube::api::PatchParams::apply(crate::api::FIELD_MANAGER).force(), &kube::api::Patch::Apply(body))
                .await
            {
                Ok(_) => Ok(()),
                Err(error) => {
                    tracing::warn!(%error, %namespace, %name, "applying copy failed");
                    Err(error)
                }
            }
        }
        Step::Delete { namespace, name } => match copies(client, kind, namespace).delete(name, &kube::api::DeleteParams::default()).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(status)) if status.is_not_found() => Ok(()),
            Err(error) => {
                tracing::warn!(%error, %namespace, %name, "deleting copy failed");
                Err(error)
            }
        },
    }
}

async fn set_status(api: &kube::Api<crate::api::Flect>, flect: &crate::api::Flect, outcome: &Outcome) -> Result<bool, kube::Error> {
    use kube::ResourceExt;
    let status = crate::api::FlectStatus { synced: Some(outcome.synced()), message: Some(outcome.message()) };
    if flect.status.as_ref() == Some(&status) {
        return Ok(false);
    }
    api.patch_status(&flect.name_any(), &kube::api::PatchParams::default(), &kube::api::Patch::Merge(serde_json::json!({ "status": status })))
        .await?;
    Ok(true)
}

async fn record(ctx: &Ctx, flect: &crate::api::Flect, type_: kube::runtime::events::EventType, reason: &str, note: &str) {
    use kube::Resource;
    let event = kube::runtime::events::Event {
        type_,
        reason: reason.to_string(),
        note: Some(note.to_string()),
        action: "Sync".to_string(),
        secondary: None,
    };
    if let Err(error) = ctx.recorder.publish(&event, &flect.object_ref(&())).await {
        tracing::warn!(%error, "publishing event failed");
    }
}

fn copies(client: &kube::Client, kind: crate::api::SourceKind, namespace: &str) -> kube::Api<kube::api::DynamicObject> {
    kube::Api::namespaced_with(
        client.clone(),
        namespace,
        &kube::api::ApiResource::from_gvk(&kube::api::GroupVersionKind::gvk(
            "",
            "v1",
            match kind {
                crate::api::SourceKind::ConfigMap => "ConfigMap",
                crate::api::SourceKind::Secret => "Secret",
            },
        )),
    )
}
