use super::*;
use std::cell::Cell;

#[test]
fn failed_model_client_build_can_retry_and_only_success_is_cached() {
    let hub = RuntimeServerClientHub::default();
    let builds = Cell::new(0);
    let failed = hub.model_http_client_with(|| {
        builds.set(builds.get() + 1);
        Err("injected build failure".into())
    });
    assert_eq!(failed.err().unwrap(), "injected build failure");
    hub.model_http_client_with(|| {
        builds.set(builds.get() + 1);
        crate::http_transport::build_model_http_client()
    })
    .unwrap();
    hub.model_http_client_with(|| panic!("successful client must already be cached"))
        .unwrap();
    assert_eq!(builds.get(), 2);
}

#[test]
fn background_and_unregistered_writers_share_the_runtime_owned_model_client() {
    let hub = Arc::new(RuntimeServerClientHub::default());
    hub.broadcaster().model_http_client().unwrap();
    hub.process_scheduler().model_http_client().unwrap();
    hub.model_http_client_with(|| panic!("background writers must share the hub pool"))
        .unwrap();
    let (writer, _events) = hub.connect().unwrap().unwrap();
    writer.model_http_client().unwrap();
    hub.model_http_client_with(|| panic!("EventWriter must share its hub client"))
        .unwrap();
}

#[test]
fn concurrent_first_model_client_access_builds_once() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    };
    let hub = RuntimeServerClientHub::default();
    let builds = AtomicUsize::new(0);
    let barrier = Barrier::new(5);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                barrier.wait();
                hub.model_http_client_with(|| {
                    builds.fetch_add(1, Ordering::SeqCst);
                    crate::http_transport::build_model_http_client()
                })
                .unwrap();
            });
        }
        barrier.wait();
    });
    assert_eq!(builds.load(Ordering::SeqCst), 1);
}
