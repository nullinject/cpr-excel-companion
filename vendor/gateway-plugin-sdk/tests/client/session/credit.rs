//! 流信用等待、迟到信用、分块上限与资源流期限测试

use super::*;

#[tokio::test]
async fn stream_waits_for_credit_and_rejects_uncreditable_buffered_chunk_before_result() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "stream", json!({}), Vec::new()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), read_frame(&mut host.reader))
            .await
            .is_err()
    );
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 4,
            frames: 1,
        },
    )
    .await;

    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    assert!(matches!(
        receive(&mut host).await,
        Frame {
            message: Message::Stream { id: 1, sequence: 0 },
            payload,
        } if payload == b"abcd"
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), read_frame(&mut host.reader))
            .await
            .is_err()
    );
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 4,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await,
        Frame {
            message: Message::Stream { id: 1, sequence: 1 },
            payload,
        } if payload == b"efgh"
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));

    send_call(&mut host, 3, "oversized_stream", json!({}), Vec::new()).await;
    send_control(
        &mut host,
        Message::Credit {
            id: 3,
            bytes: 4,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 3,
            error: PluginFault {
                code: ErrorCode::InvalidInput,
                ..
            },
        }
    ));

    send_call(&mut host, 5, "too_many_chunks", json!({}), Vec::new()).await;
    send_control(
        &mut host,
        Message::Credit {
            id: 5,
            bytes: 4,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 5,
            error: PluginFault {
                code: ErrorCode::Capacity,
                ..
            },
        }
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn late_credit_after_a_long_stream_terminal_does_not_close_the_session() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "long_stream", json!({}), Vec::new()).await;
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    for sequence in 0..16 {
        assert!(matches!(
            receive(&mut host).await,
            Frame {
                message: Message::Stream { id: 1, sequence: observed },
                payload,
            } if observed == sequence && payload == b"x"
        ));
        send_control(
            &mut host,
            Message::Credit {
                id: 1,
                bytes: 1,
                frames: 1,
            },
        )
        .await;
    }
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));

    send_call(&mut host, 3, "echo", json!("still-ready"), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 3, result } if result == json!("still-ready")
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn malformed_late_credit_remains_a_protocol_error() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "echo", json!({}), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));

    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 0,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("protocol error did not close the session")
            .expect("plugin session task panicked"),
        Err(SessionError::Protocol)
    ));
}

#[tokio::test]
async fn dynamic_stream_ends_with_one_error_when_a_later_chunk_cannot_fit_credit() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(
        &mut host,
        1,
        "dynamic_oversized_stream",
        json!({}),
        Vec::new(),
    )
    .await;
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 4,
            frames: 1,
        },
    )
    .await;

    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End {
            id: 1,
            error: Some(PluginFault {
                code: ErrorCode::InvalidInput,
                ..
            }),
        }
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn resource_stream_keeps_callbacks_and_credit_alive_after_the_initial_deadline() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    let mut context = context(1, Duration::from_millis(500));
    context.resource_stream = true;
    write_frame(
        &mut host.writer,
        &Frame::control(Message::Call {
            id: 1,
            method: "resource_stream".into(),
            context,
            params: json!({}),
        }),
    )
    .await
    .unwrap();
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 16,
            frames: 2,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    let Message::Callback {
        id, parent_id: 1, ..
    } = receive(&mut host).await.message
    else {
        panic!("stream callback must survive initial timeout");
    };
    send_control(
        &mut host,
        Message::Result {
            id,
            result: json!({}),
        },
    )
    .await;
    assert!(
        matches!(receive(&mut host).await, Frame { message: Message::Stream { id: 1, sequence: 0 }, payload } if payload == b"live")
    );
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));
    shutdown(&mut host, task).await;
}
