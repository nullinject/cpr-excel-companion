//! 二进制帧往返、长度预算与不完整帧拒绝测试

use gateway_plugin_sdk::{
    Frame, FrameError, Message,
    client::{read_frame, write_frame},
};
use serde_json::json;

#[tokio::test]
async fn binary_payload_round_trips_without_json_encoding() {
    let frame = Frame {
        message: Message::Stream {
            id: 17,
            sequence: 2,
        },
        payload: vec![0, 255, 13, 10, 128],
    };
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &frame).await.unwrap();
    assert!(bytes.ends_with(&frame.payload));
    assert_eq!(read_frame(&mut bytes.as_slice()).await.unwrap(), frame);
}

#[tokio::test]
async fn malicious_lengths_fail_before_reading_or_allocating_payload() {
    let bytes = [0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(matches!(
        read_frame(&mut bytes.as_slice()).await,
        Err(FrameError::Length)
    ));
}

#[tokio::test]
async fn large_payload_is_exact_across_multiple_io_buffers_and_following_frames() {
    let frame = Frame {
        message: Message::Result {
            id: 1,
            result: json!({}),
        },
        payload: (0..101 * 1024 * 1024 + 13)
            .map(|index| (index % 251) as u8)
            .collect(),
    };
    let next = Frame::control(Message::Cancel { id: 3 });
    let (mut writer, mut reader) = tokio::io::duplex(16 * 1024);
    let writing = tokio::spawn(async move {
        write_frame(&mut writer, &frame).await.unwrap();
        write_frame(&mut writer, &next).await.unwrap();
        (frame, next)
    });
    let received = read_frame(&mut reader).await.unwrap();
    let following = read_frame(&mut reader).await.unwrap();
    let (original, next) = writing.await.unwrap();
    assert_eq!(received, original);
    assert_eq!(following, next);
}

#[tokio::test]
async fn truncated_large_payload_never_becomes_a_successful_message() {
    let message = serde_json::to_vec(&Message::Stream { id: 1, sequence: 0 }).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(message.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&(101_u64 * 1024 * 1024).to_be_bytes());
    bytes.extend_from_slice(&message);
    bytes.extend_from_slice(&[1, 2, 3]);
    assert!(matches!(
        read_frame(&mut bytes.as_slice()).await,
        Err(FrameError::Io(_))
    ));
}

#[tokio::test]
async fn business_json_uses_payload_prefix_for_configuration_calls_callbacks_and_results() {
    let data =
        json!({"value": "x".repeat(256 * 1024), "empty": null, "zero": 0, "disabled": false});
    let messages = [
        Message::Hello {
            handshake: gateway_plugin_sdk::Handshake {
                protocol_version: gateway_plugin_sdk::PROTOCOL_VERSION,
                artifact_sha256: "a".repeat(64),
                plugin_id: "test.example".into(),
                instance_id: "test-instance".into(),
                generation: 1,
                incarnation: "test-incarnation".into(),
                configuration: data.clone(),
                contributes: Default::default(),
            },
        },
        Message::Call {
            id: 1,
            method: "policy.schedule_account".into(),
            context: super::session::context(1, std::time::Duration::from_secs(5)),
            params: data.clone(),
        },
        Message::Callback {
            id: 2,
            parent_id: 1,
            method: "host.state.put".into(),
            params: data.clone(),
        },
        Message::Result {
            id: 2,
            result: data,
        },
    ];
    for message in messages {
        let frame = Frame {
            message,
            payload: vec![0, 255, 128, 42],
        };
        let encoded = gateway_plugin_sdk::client::EncodedFrame::new(frame.clone()).unwrap();
        let mut bytes = Vec::new();
        encoded.write(&mut bytes).await.unwrap();
        let metadata_length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert!(metadata_length < 1024);
        let metadata: serde_json::Value =
            serde_json::from_slice(&bytes[12..12 + metadata_length]).unwrap();
        assert!(metadata["data_bytes"].as_u64().unwrap() > 256 * 1024);
        assert!(metadata.get("params").is_none());
        assert!(metadata.get("result").is_none());
        assert!(metadata["handshake"].get("configuration").is_none());
        let next = Frame::control(Message::Cancel { id: 3 });
        write_frame(&mut bytes, &next).await.unwrap();
        let mut reader = bytes.as_slice();
        assert_eq!(read_frame(&mut reader).await.unwrap(), frame);
        assert_eq!(read_frame(&mut reader).await.unwrap(), next);
        assert!(reader.is_empty());
    }
}

#[tokio::test]
async fn data_prefix_lengths_cannot_consume_the_following_frame() {
    for (message, request_bytes, data_bytes, payload_bytes) in [
        (Message::Cancel { id: 1 }, 0, 100, 100),
        (
            Message::Result {
                id: 1,
                result: json!(null),
            },
            0,
            101,
            100,
        ),
        (
            Message::Result {
                id: 1,
                result: json!(null),
            },
            0,
            0,
            0,
        ),
        (
            Message::Call {
                id: 1,
                method: "echo".into(),
                context: super::session::context(1, std::time::Duration::from_secs(5)),
                params: json!(null),
            },
            11,
            10,
            20,
        ),
    ] {
        let mut metadata = serde_json::to_value(message).unwrap();
        metadata["request_context_bytes"] = json!(request_bytes);
        metadata["data_bytes"] = json!(data_bytes);
        let metadata = serde_json::to_vec(&metadata).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(payload_bytes as u64).to_be_bytes());
        bytes.extend_from_slice(&metadata);
        let next = Frame::control(Message::Cancel { id: 3 });
        write_frame(&mut bytes, &next).await.unwrap();
        let mut reader = bytes.as_slice();
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(FrameError::Length)
        ));
        assert_eq!(read_frame(&mut reader).await.unwrap(), next);
    }
}

#[tokio::test]
async fn deeply_nested_business_json_is_rejected_within_the_parser_stack_budget() {
    let data = format!("{}0{}", "[".repeat(4096), "]".repeat(4096));
    let metadata = serde_json::to_vec(&json!({
        "type": "result",
        "id": 1,
        "data_bytes": data.len(),
    }))
    .unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&(data.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&metadata);
    bytes.extend_from_slice(data.as_bytes());
    assert!(matches!(
        read_frame(&mut bytes.as_slice()).await,
        Err(FrameError::Metadata(error)) if error.is_syntax()
    ));
}

#[tokio::test]
async fn truncated_frame_never_becomes_a_successful_message() {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &Frame::control(Message::Cancel { id: 3 }))
        .await
        .unwrap();
    bytes.pop();
    assert!(matches!(
        read_frame(&mut bytes.as_slice()).await,
        Err(FrameError::Io(_))
    ));
}

#[tokio::test]
async fn request_context_uses_payload_prefix_without_consuming_metadata_budget_or_body() {
    let mut context = super::session::context(1, std::time::Duration::from_secs(5));
    context.request = Some(std::sync::Arc::new(gateway_plugin_sdk::RequestContext {
        method: "POST".into(),
        uri: "/v1/responses?fixture-secret".into(),
        http_version: "HTTP/1.1".into(),
        headers: vec![
            gateway_plugin_sdk::call::middleware::MiddlewareHeader {
                name: "x-repeat".into(),
                value: vec![255; 60_000],
            },
            gateway_plugin_sdk::call::middleware::MiddlewareHeader {
                name: "x-repeat".into(),
                value: (0..=255).collect(),
            },
            gateway_plugin_sdk::call::middleware::MiddlewareHeader {
                name: "x-empty".into(),
                value: vec![],
            },
        ],
        peer_addr: Some("[2001:db8::1]:1234".into()),
        client_ip: Some("203.0.113.7".into()),
    }));
    assert!(!format!("{context:?}").contains("fixture-secret"));
    let frame = Frame {
        message: Message::Call {
            id: 1,
            method: "observer.observe".into(),
            context,
            params: json!({}),
        },
        payload: vec![42; 1024 * 1024],
    };
    gateway_plugin_sdk::client::validate_frame(&frame).unwrap();
    let encoded = gateway_plugin_sdk::client::EncodedFrame::new(frame.clone()).unwrap();
    let next = Frame::control(Message::Cancel { id: 1 });
    let mut bytes = Vec::new();
    encoded.write(&mut bytes).await.unwrap();
    let mut direct = Vec::new();
    write_frame(&mut direct, &frame).await.unwrap();
    assert_eq!(bytes, direct);
    let metadata_length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    assert!(metadata_length < 1024);
    let metadata: serde_json::Value =
        serde_json::from_slice(&bytes[12..12 + metadata_length]).unwrap();
    assert!(metadata["request_context_bytes"].as_u64().unwrap() > 64 * 1024);
    assert!(metadata["context"].get("request").is_none());
    let request_start = 12 + metadata_length;
    let request_end = request_start + metadata["request_context_bytes"].as_u64().unwrap() as usize;
    let request: serde_json::Value =
        serde_json::from_slice(&bytes[request_start..request_end]).unwrap();
    assert_eq!(
        request["headers"][0]["value"].as_str().unwrap().len(),
        80_000
    );
    assert_eq!(request["headers"][1]["value"].as_str().unwrap().len(), 344);
    assert_eq!(request["headers"][2]["value"], "");
    write_frame(&mut bytes, &next).await.unwrap();
    let mut reader = bytes.as_slice();
    assert_eq!(read_frame(&mut reader).await.unwrap(), frame);
    assert_eq!(read_frame(&mut reader).await.unwrap(), next);
    assert!(reader.is_empty());
}

#[test]
fn request_context_header_values_accept_json_escapes_and_reject_invalid_base64() {
    let json = r#"{
        "method":"GET","uri":"/","http_version":"HTTP/1.1",
        "headers":[{"name":"x-binary","value":"\u0041P8="}],
        "peer_addr":null,"client_ip":null
    }"#;
    let request: gateway_plugin_sdk::RequestContext = serde_json::from_str(json).unwrap();
    assert_eq!(request.headers[0].value, [0, 255]);
    let invalid = json.replace("\\u0041P8=", "not base64");
    assert!(serde_json::from_str::<gateway_plugin_sdk::RequestContext>(&invalid).is_err());
    // 入口快照的紧凑表示不改变中间件业务数据的头值合同
    assert_eq!(
        serde_json::to_value(&request.headers[0]).unwrap()["value"],
        json!([0, 255])
    );
}

#[tokio::test]
async fn context_prefix_cannot_exceed_payload_or_appear_on_non_call_messages() {
    for payload_length in [0_u64, 100] {
        let mut metadata = serde_json::to_value(Message::Cancel { id: 1 }).unwrap();
        metadata["request_context_bytes"] = json!(100);
        let metadata = serde_json::to_vec(&metadata).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&payload_length.to_be_bytes());
        bytes.extend_from_slice(&metadata);
        assert!(matches!(
            read_frame(&mut bytes.as_slice()).await,
            Err(FrameError::Length)
        ));
    }
}
