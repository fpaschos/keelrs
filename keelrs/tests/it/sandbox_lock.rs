//! Per-file locks through the public API: `Sandbox::lock` and `lock_all`
//! on a local sandbox, each test on its own temp dir.

use std::future::Future;
use std::time::Duration;

use keelrs::Error;
use keelrs::sandbox::SandboxError;

use crate::support::{open, temp_dir};

/// `work`, or a panic naming `what` if it takes over a second.
async fn within<T>(what: &str, work: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(1), work)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish"))
}

#[tokio::test]
async fn a_lock_out_of_the_work_dir_is_refused_and_holds_nothing() {
    let dir = temp_dir();
    let sb = open(&dir).await;

    let refused = sb.lock("../outside").await;
    assert!(
        matches!(
            refused,
            Err(Error::Sandbox(SandboxError::OutsideWorkDir { .. }))
        ),
        "{:?}",
        refused.err()
    );
    let refused = sb.lock_all(["f", "../outside"]).await;
    assert!(refused.is_err());
    let _held = within("locking f after a refused lock_all", sb.lock("f"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_locked_file_waits_for_its_holder_on_every_clone() {
    let dir = temp_dir();
    let sb = open(&dir).await;
    let held = sb.lock("sub/f").await.unwrap();

    let sub = sb.work_dir("sub");
    let waiting = tokio::time::timeout(Duration::from_millis(50), sub.lock("f")).await;
    assert!(waiting.is_err(), "a second holder got sub/f");
    // Another file is not held.
    let _held = within("locking another file", sub.lock("g")).await.unwrap();
    drop(held);
    let _held = within("locking f once released", sub.lock("f"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_lock_names_its_files_canonical_in_the_order_given() {
    let dir = temp_dir();
    let sb = open(&dir).await;

    let held = sb.lock_all(["b", "sub/../a", "./b"]).await.unwrap();
    let root = dir.to_str().unwrap();
    let paths: Vec<&str> = held.paths().iter().map(|path| path.as_str()).collect();
    assert_eq!(
        paths,
        [
            format!("{root}/b"),
            format!("{root}/a"),
            format!("{root}/b")
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_all_in_opposite_orders_does_not_deadlock() {
    let dir = temp_dir();
    let sb = open(&dir).await;
    for _ in 0..50 {
        let (one, two) = (sb.clone(), sb.clone());
        let one = tokio::spawn(async move {
            let _held = one.lock_all(["a", "b", "a"]).await.unwrap();
            tokio::task::yield_now().await;
        });
        let two = tokio::spawn(async move {
            let _held = two.lock_all(["b", "a"]).await.unwrap();
            tokio::task::yield_now().await;
        });
        within("two opposite lock_alls", async {
            one.await.unwrap();
            two.await.unwrap();
        })
        .await;
    }
}
