use crate::gateway::{Gateway, GatewayListeners, GatewayListenersTlsCertificateRefs};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    api::{Api, ListParams, Patch, PatchParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        events::{Event, EventType, Recorder, Reporter},
        finalizer::{finalizer, Event as Finalizer},
        watcher::Config,
    },
    CustomResource, Resource,
};
use log::{error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Duration};

pub static DOCUMENT_FINALIZER: &str = "bcerts.aruna-storage.org";

/// Generate the Kubernetes wrapper struct `BucketCert` from our Spec and Status struct
///
/// This provides a hook for generating the CRD yaml (in crdgen.rs)
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "BucketCert",
    group = "aruna-storage.org",
    version = "v1alpha1",
    namespaced
)]
#[kube(status = "BucketCertStatus", shortname = "bcert")]
pub struct BucketCertSpec {
    pub bucket: String,
}
/// The status object of `BucketCert`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct BucketCertStatus {
    pub compacted: bool,
    pub created: bool,
}

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
}

async fn reconcile(bcert: Arc<BucketCert>, ctx: Arc<Context>) -> Result<Action> {
    ctx.diagnostics.write().await.last_event = Utc::now();
    let ns = bcert.namespace().unwrap(); // doc is namespace scoped
    let bcerts: Api<BucketCert> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling BucketCert \"{}\" in {}", bcert.name_any(), ns);
    finalizer(&bcerts, DOCUMENT_FINALIZER, bcert, |event| async {
        match event {
            Finalizer::Apply(bcert) => bcert.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(bcert) => bcert.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

fn error_policy(_doc: Arc<BucketCert>, error: &Error, _ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

impl BucketCert {
    // Reconcile (for non-finalizer related changes)
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        let client = ctx.client.clone();
        let recorder = ctx.diagnostics.read().await.recorder(client.clone(), self);
        let ns = self.namespace().unwrap();
        let name = self.name_any();
        let bcerts: Api<BucketCert> = Api::namespaced(client.clone(), &ns);
        let gateway: Api<Gateway> = Api::namespaced(client, &ns);

        let bucket_name = self.spec.bucket.to_string();

        match &self.status {
            Some(status) => {
                if status.created == true {
                } else {
                    let query_gw = gateway.get("eg").await.map_err(Error::KubeError)?;
                    let mut new_listeners = query_gw.spec.listeners.clone();
                    new_listeners.push(GatewayListeners {
                        allowed_routes: None,
                        hostname: Some(format!("{bucket_name}.data.gi.aruna-storage.org")),
                        name: bucket_name.to_string(),
                        port: 443,
                        protocol: "HTTPS".to_string(),
                        tls: Some(crate::gateway::GatewayListenersTls {
                            certificate_refs: Some(vec![GatewayListenersTlsCertificateRefs {
                                group: None,
                                kind: Some("Secret".to_string()),
                                name: format!("{bucket_name}"),
                                namespace: Some(ns),
                            }]),
                            mode: Some(crate::gateway::GatewayListenersTlsMode::Terminate),
                            options: None,
                        }),
                    });
                    // Update gateway
                    let new_listener = Patch::Apply(json!({
                        "apiVersion": "gateway.networking.k8s.io/v1alpha2",
                        "kind": "Gateway",
                        "spec": {
                            "listeners": new_listeners
                        }
                    }));
                    let ps = PatchParams::apply("cntrlr");
                    let _o = gateway
                        .patch("eg", &ps, &new_listener) // For now the name is static eg
                        .await
                        .map_err(Error::KubeError)?;

                    recorder
                        .publish(Event {
                            type_: EventType::Normal,
                            reason: "Created entry".into(),
                            note: Some(format!("Created cert entry for `{name}`")),
                            action: "Created".into(),
                            secondary: None,
                        })
                        .await
                        .map_err(Error::KubeError)?;
                }
            }
            None => {
                let query_gw = gateway.get("eg").await.map_err(Error::KubeError)?;
                let mut new_listeners = query_gw.spec.listeners.clone();
                new_listeners.push(GatewayListeners {
                    allowed_routes: None,
                    hostname: Some(format!("{bucket_name}.data.gi.aruna-storage.org")),
                    name: bucket_name.to_string(),
                    port: 443,
                    protocol: "HTTPS".to_string(),
                    tls: Some(crate::gateway::GatewayListenersTls {
                        certificate_refs: Some(vec![GatewayListenersTlsCertificateRefs {
                            group: None,
                            kind: Some("Secret".to_string()),
                            name: format!("{bucket_name}"),
                            namespace: Some(ns),
                        }]),
                        mode: Some(crate::gateway::GatewayListenersTlsMode::Terminate),
                        options: None,
                    }),
                });
                // Update gateway
                let new_listener = Patch::Apply(json!({
                    "apiVersion": "gateway.networking.k8s.io/v1alpha2",
                    "kind": "Gateway",
                    "spec": {
                        "listeners": new_listeners
                    }
                }));
                let ps = PatchParams::apply("cntrlr");
                let _o = gateway
                    .patch("eg", &ps, &new_listener) // For now the name is static eg
                    .await
                    .map_err(Error::KubeError)?;
                recorder
                    .publish(Event {
                        type_: EventType::Normal,
                        reason: "Created entry".into(),
                        note: Some(format!("Created cert entry for `{name}`")),
                        action: "Created".into(),
                        secondary: None,
                    })
                    .await
                    .map_err(Error::KubeError)?;
            }
        }
        if name == "illegal" {
            return Err(Error::IllegalDocument); // error names show up in metrics
        }
        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "aruna-storage.org/v1alpha1",
            "kind": "BucketCert",
            "status": BucketCertStatus {
                compacted: false,
                created: true,
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = bcerts
            .patch_status(&name, &ps, &new_status)
            .await
            .map_err(Error::KubeError)?;

        // If no events were received, check back every 60 minutes
        Ok(Action::requeue(Duration::from_secs(60 * 60)))
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        let recorder = ctx
            .diagnostics
            .read()
            .await
            .recorder(ctx.client.clone(), self);
        // Document doesn't have any real cleanup, so we just publish an event
        recorder
            .publish(Event {
                type_: EventType::Normal,
                reason: "DeleteRequested".into(),
                note: Some(format!("Delete `{}`", self.name_any())),
                action: "Deleting".into(),
                secondary: None,
            })
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }
}

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "s3bto-controller".into(),
        }
    }
}
impl Diagnostics {
    fn recorder(&self, client: Client, doc: &BucketCert) -> Recorder {
        Recorder::new(client, self.reporter.clone(), doc.object_ref(&()))
    }
}

/// Initialize the controller and shared state (given the crd is installed)
pub async fn run() {
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
    let bcert = Api::<BucketCert>::all(client.clone());
    if let Err(e) = bcert.list(&ListParams::default().limit(1)).await {
        error!("CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    let gw = Api::<Gateway>::all(client.clone());
    if let Err(e) = gw.list(&ListParams::default().limit(1)).await {
        error!("Gateway CRD is not queryable; {e:?}. Is the CRD installed?");
        std::process::exit(1);
    }
    Controller::new(bcert, Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            reconcile,
            error_policy,
            Arc::new(Context {
                client,
                diagnostics: Arc::new(RwLock::new(Diagnostics::default())),
            }),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()))
        .await;
}

// // Mock tests relying on fixtures.rs and its primitive apiserver mocks
// #[cfg(test)]
// mod test {
//     use super::{error_policy, reconcile, Context, Document};
//     use crate::fixtures::{timeout_after_1s, Scenario};
//     use std::sync::Arc;

//     #[tokio::test]
//     async fn documents_without_finalizer_gets_a_finalizer() {
//         let (testctx, fakeserver, _) = Context::test();
//         let doc = Document::test();
//         let mocksrv = fakeserver.run(Scenario::FinalizerCreation(doc.clone()));
//         reconcile(Arc::new(doc), testctx).await.expect("reconciler");
//         timeout_after_1s(mocksrv).await;
//     }

//     #[tokio::test]
//     async fn finalized_doc_causes_status_patch() {
//         let (testctx, fakeserver, _) = Context::test();
//         let doc = Document::test().finalized();
//         let mocksrv = fakeserver.run(Scenario::StatusPatch(doc.clone()));
//         reconcile(Arc::new(doc), testctx).await.expect("reconciler");
//         timeout_after_1s(mocksrv).await;
//     }

//     #[tokio::test]
//     async fn finalized_doc_with_hide_causes_event_and_hide_patch() {
//         let (testctx, fakeserver, _) = Context::test();
//         let doc = Document::test().finalized().needs_hide();
//         let scenario = Scenario::EventPublishThenStatusPatch("HideRequested".into(), doc.clone());
//         let mocksrv = fakeserver.run(scenario);
//         reconcile(Arc::new(doc), testctx).await.expect("reconciler");
//         timeout_after_1s(mocksrv).await;
//     }

//     #[tokio::test]
//     async fn finalized_doc_with_delete_timestamp_causes_delete() {
//         let (testctx, fakeserver, _) = Context::test();
//         let doc = Document::test().finalized().needs_delete();
//         let mocksrv = fakeserver.run(Scenario::Cleanup("DeleteRequested".into(), doc.clone()));
//         reconcile(Arc::new(doc), testctx).await.expect("reconciler");
//         timeout_after_1s(mocksrv).await;
//     }

//     #[tokio::test]
//     async fn illegal_doc_reconcile_errors_which_bumps_failure_metric() {
//         let (testctx, fakeserver, _registry) = Context::test();
//         let doc = Arc::new(Document::illegal().finalized());
//         let mocksrv = fakeserver.run(Scenario::RadioSilence);
//         let res = reconcile(doc.clone(), testctx.clone()).await;
//         timeout_after_1s(mocksrv).await;
//         assert!(res.is_err(), "apply reconciler fails on illegal doc");
//         let err = res.unwrap_err();
//         assert!(err.to_string().contains("IllegalDocument"));
//         // calling error policy with the reconciler error should cause the correct metric to be set
//         error_policy(doc.clone(), &err, testctx.clone());
//         //dbg!("actual metrics: {}", registry.gather());
//         let failures = testctx
//             .metrics
//             .failures
//             .with_label_values(&["illegal", "finalizererror(applyfailed(illegaldocument))"])
//             .get();
//         assert_eq!(failures, 1);
//     }

//     // Integration test without mocks
//     use kube::api::{Api, ListParams, Patch, PatchParams};
//     #[tokio::test]
//     #[ignore = "uses k8s current-context"]
//     async fn integration_reconcile_should_set_status_and_send_event() {
//         let client = kube::Client::try_default().await.unwrap();
//         let ctx = super::State::default().to_context(client.clone());

//         // create a test doc
//         let doc = Document::test().finalized().needs_hide();
//         let docs: Api<Document> = Api::namespaced(client.clone(), "default");
//         let ssapply = PatchParams::apply("ctrltest");
//         let patch = Patch::Apply(doc.clone());
//         docs.patch("test", &ssapply, &patch).await.unwrap();

//         // reconcile it (as if it was just applied to the cluster like this)
//         reconcile(Arc::new(doc), ctx).await.unwrap();

//         // verify side-effects happened
//         let output = docs.get_status("test").await.unwrap();
//         assert!(output.status.is_some());
//         // verify hide event was found
//         let events: Api<k8s_openapi::api::core::v1::Event> = Api::all(client.clone());
//         let opts =
//             ListParams::default().fields("involvedObject.kind=Document,involvedObject.name=test");
//         let event = events
//             .list(&opts)
//             .await
//             .unwrap()
//             .into_iter()
//             .filter(|e| e.reason.as_deref() == Some("HideRequested"))
//             .last()
//             .unwrap();
//         dbg!("got ev: {:?}", &event);
//         assert_eq!(event.action.as_deref(), Some("Hiding"));
//     }
// }
