mod api;
mod controller;

use crate::api::Flect;
use crate::api::SOURCE_ANNOTATION;
use crate::api::SourceKind;
use crate::controller::Ctx;
use crate::controller::affected;
use crate::controller::error_policy;
use crate::controller::reconcile;
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use kube::Client;
use kube::CustomResourceExt;
use kube::Resource;
use kube::ResourceExt;
use kube::api::ListParams;
use kube::runtime::controller::Controller;
use kube::runtime::events::Recorder;
use kube::runtime::reflector;
use kube::runtime::reflector::ObjectRef;
use kube::runtime::watcher;
use serde::de::DeserializeOwned;
use std::fmt::Debug;
use std::sync::Arc;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), kube::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,kube=warn")))
        .init();

    if std::env::args().nth(1).as_deref() == Some("crd") {
        println!("{}", serde_json::to_string_pretty(&Flect::crd()).unwrap());
        return Ok(());
    }

    let client = Client::try_default().await?;
    require_crd(&client).await?;
    let recorder = Recorder::new(client.clone(), "flector".into());
    let (flects, writer) = reflector::store::<Flect>();

    tokio::spawn(reflector(writer, watcher(Api::<Flect>::all(client.clone()), watcher::Config::default())).for_each(|event| async move {
        if let Err(error) = event {
            warn!(%error, "watching flects failed");
        }
    }));

    let config_maps = mapper(flects.clone(), SourceKind::ConfigMap);
    let secrets = mapper(flects.clone(), SourceKind::Secret);

    info!("flector started");
    Controller::new(Api::<Flect>::all(client.clone()), watcher::Config::default())
        .watches_with(Api::<ConfigMap>::all(client.clone()), (), watcher::Config::default(), config_maps)
        .watches_with(Api::<Secret>::all(client.clone()), (), watcher::Config::default(), secrets)
        .shutdown_on_signal()
        .run(reconcile, error_policy, Arc::new(Ctx { client, recorder }))
        .for_each(|result| async move {
            match result {
                Ok(_) => {}
                Err(kube::runtime::controller::Error::ObjectNotFound { .. }) => {}
                Err(kube::runtime::controller::Error::ReconcilerFailed(kube::Error::Api(status), _)) if status.is_not_found() => {}
                Err(error) => warn!(%error, "controller failed"),
            }
        })
        .await;
    Ok(())
}

fn mapper<K>(flects: reflector::Store<Flect>, kind: SourceKind) -> impl Fn(K) -> Vec<ObjectRef<Flect>> + Send + Sync + 'static
where
    K: Resource<DynamicType = ()> + ResourceExt + DeserializeOwned + Clone + Debug + Send + 'static,
{
    move |object: K| {
        affected(
            &flects.state(),
            kind,
            object.namespace().as_deref().unwrap_or_default(),
            &object.name_any(),
            object.annotations().get(SOURCE_ANNOTATION).map(String::as_str),
        )
    }
}

async fn require_crd(client: &Client) -> Result<(), kube::Error> {
    Api::<Flect>::all(client.clone()).list(&ListParams::default().limit(1)).await.map(|_| ())
}
