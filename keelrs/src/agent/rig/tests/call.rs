//! One call through the crate-private `rig::call`, the seam `answer` runs it with:
//! arguments, decode errors and timeouts no public seam reaches alone.

use super::*;

#[tokio::test]
async fn a_call_gets_its_arguments_and_id_and_settles_to_its_value() {
    let called = call(
        &lookup_order(),
        json!({"order_id": "A1"}),
        "c1".into(),
        &unused(),
    )
    .await;
    assert_eq!(called, Ok(json!({"order": "A1", "call": "c1"})));
}

#[tokio::test]
async fn an_app_error_fails_the_call_as_other_with_its_message() {
    let tool = Tool::host(
        "lookup_order",
        "Look up an order.",
        async |call: ToolCall<Order>| Err::<(), _>(format!("no order {}", call.data.order_id)),
    );
    let called = call(&tool, json!({"order_id": "Z9"}), "c1".into(), &unused()).await;
    assert_eq!(called, Err(Failed::new(FailKind::Other, "no order Z9")));
}

#[tokio::test]
async fn arguments_that_break_the_schema_are_invalid_args_and_the_tool_does_not_run() {
    let ran = Arc::new(AtomicBool::new(false));
    let tool = Tool::host("lookup_order", "Look up an order.", {
        let ran = ran.clone();
        async move |_: ToolCall<Order>| {
            ran.store(true, Ordering::SeqCst);
            Ok::<_, Infallible>("found")
        }
    });
    let failed = call(&tool, json!({"order_id": 5}), "c1".into(), &unused())
        .await
        .unwrap_err();
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(failed.kind, FailKind::InvalidArgs);
    assert!(failed.message.contains("do not fit its schema"), "{failed}");
}

#[derive(Deserialize, JsonSchema)]
struct Ping {
    /// The schema takes any string; only an IPv4 address decodes.
    #[schemars(with = "String")]
    host: std::net::Ipv4Addr,
}

#[tokio::test]
async fn arguments_the_schema_takes_but_that_do_not_decode_are_invalid_args() {
    let tool = Tool::host("ping", "Ping a host.", async |call: ToolCall<Ping>| {
        Ok::<_, Infallible>(call.data.host.to_string())
    });
    let failed = call(&tool, json!({"host": "nowhere"}), "c1".into(), &unused())
        .await
        .unwrap_err();
    assert_eq!(failed.kind, FailKind::InvalidArgs);
    assert!(
        failed
            .message
            .starts_with("`ping`'s arguments do not fit: "),
        "{failed}"
    );
}

#[tokio::test]
async fn a_call_past_its_timeout_fails_as_timed_out() {
    let tool = Tool::host("slow", "Never finishes.", async |_: ToolCall<()>| {
        std::future::pending::<Result<(), Infallible>>().await
    })
    .timeout(Duration::from_millis(10));
    let called = call(&tool, json!({}), "c1".into(), &unused()).await;
    assert_eq!(
        called,
        Err(Failed::new(
            FailKind::Timeout,
            "tool \"slow\" timed out after 10ms"
        ))
    );
}

#[test]
fn a_failed_call_reaches_rig_with_its_kind_and_message() {
    for (kind, rig) in [
        (FailKind::InvalidArgs, ToolErrorKind::InvalidArgs),
        (FailKind::NotFound, ToolErrorKind::NotFound),
        (FailKind::PermissionDenied, ToolErrorKind::PermissionDenied),
        (FailKind::Timeout, ToolErrorKind::Timeout),
        (FailKind::Other, ToolErrorKind::Other),
    ] {
        let answered = result(Err(Failed::new(kind, "why")));
        let error = answered.error().expect("a failure");
        assert_eq!((error.kind(), error.message()), (rig, "why"));
    }
}
