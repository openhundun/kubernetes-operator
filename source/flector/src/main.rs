mod api;
mod controller;

#[tokio::main]
async fn main() -> Result<(), kube::Error> {
    use futures::StreamExt;
    use kube::CustomResourceExt;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,kube=warn")))
        .init();

    if std::env::args().nth(1).as_deref() == Some("crd") {
        println!("{}", serde_json::to_string_pretty(&crate::api::Flect::crd()).unwrap());
        return Ok(());
    }

    let client = kube::Client::try_default().await?;
    require_crd(&client).await?;
    let recorder = kube::runtime::events::Recorder::new(client.clone(), "flector".into());
    let (flects, writer) = kube::runtime::reflector::store::<crate::api::Flect>();

    tokio::spawn(
        kube::runtime::reflector::reflector(
            writer,
            kube::runtime::watcher::watcher(kube::Api::<crate::api::Flect>::all(client.clone()), kube::runtime::watcher::Config::default()),
        )
        .for_each(|event| async move {
            if let Err(error) = event {
                tracing::warn!(%error, "watching flects failed");
            }
        }),
    );

    let flects_for_secrets = flects.clone();
    let config_maps = move |object: k8s_openapi::api::core::v1::ConfigMap| affected_by(&flects, crate::api::SourceKind::ConfigMap, &object);
    let secrets = move |object: k8s_openapi::api::core::v1::Secret| affected_by(&flects_for_secrets, crate::api::SourceKind::Secret, &object);

    tracing::info!("flector started");
    kube::runtime::controller::Controller::new(kube::Api::<crate::api::Flect>::all(client.clone()), kube::runtime::watcher::Config::default())
        .watches_with(
            kube::Api::<k8s_openapi::api::core::v1::ConfigMap>::all(client.clone()),
            (),
            kube::runtime::watcher::Config::default(),
            config_maps,
        )
        .watches_with(
            kube::Api::<k8s_openapi::api::core::v1::Secret>::all(client.clone()),
            (),
            kube::runtime::watcher::Config::default(),
            secrets,
        )
        .shutdown_on_signal()
        .run(
            crate::controller::reconcile,
            crate::controller::error_policy,
            std::sync::Arc::new(crate::controller::Ctx { client, recorder }),
        )
        .for_each(|result| async move {
            match result {
                Ok(_) => {}
                Err(kube::runtime::controller::Error::ObjectNotFound { .. }) => {}
                Err(kube::runtime::controller::Error::ReconcilerFailed(kube::runtime::finalizer::Error::ApplyFailed(kube::Error::Api(status)), _)) if status.is_not_found() => {}
                Err(error) => tracing::warn!(%error, "controller failed"),
            }
        })
        .await;
    Ok(())
}

fn affected_by<K: kube::ResourceExt>(
    flects: &kube::runtime::reflector::Store<crate::api::Flect>,
    kind: crate::api::SourceKind,
    object: &K,
) -> Vec<kube::runtime::reflector::ObjectRef<crate::api::Flect>> {
    crate::controller::affected(
        &flects.state(),
        kind,
        object.namespace().as_deref().unwrap_or_default(),
        &object.name_any(),
        object.annotations().get(crate::api::SOURCE_ANNOTATION).map(String::as_str),
    )
}

async fn require_crd(client: &kube::Client) -> Result<(), kube::Error> {
    kube::Api::<crate::api::Flect>::all(client.clone()).list(&kube::api::ListParams::default().limit(1)).await.map(|_| ())
}
