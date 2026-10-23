//! 入口快照头值的紧凑表示；不改变中间件业务合同或 Rust 字节 API

use base64::{Engine as _, display::Base64Display, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::call::middleware::MiddlewareHeader;

#[derive(Serialize)]
struct EncodedHeader<'a> {
    name: &'a str,
    #[serde(serialize_with = "serialize_value")]
    value: &'a [u8],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodedHeader {
    name: String,
    #[serde(deserialize_with = "deserialize_value")]
    value: Vec<u8>,
}

pub(super) fn serialize<S: Serializer>(
    headers: &[MiddlewareHeader],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(headers.iter().map(|header| EncodedHeader {
        name: &header.name,
        value: &header.value,
    }))
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<MiddlewareHeader>, D::Error> {
    Ok(Vec::<DecodedHeader>::deserialize(deserializer)?
        .into_iter()
        .map(|header| MiddlewareHeader {
            name: header.name,
            value: header.value,
        })
        .collect())
}

fn serialize_value<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    // 直接写入序列化器，不为每个头值再分配一份 base64 字符串
    serializer.collect_str(&Base64Display::new(value, &STANDARD))
}

fn deserialize_value<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct ValueVisitor;

    impl de::Visitor<'_> for ValueVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a base64-encoded header value")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            let mut bytes = Vec::new();
            let capacity = base64::decoded_len_estimate(value.len());
            bytes.try_reserve_exact(capacity).map_err(E::custom)?;
            bytes.resize(capacity, 0);
            let length = STANDARD
                .decode_slice(value, &mut bytes)
                .map_err(E::custom)?;
            bytes.truncate(length);
            Ok(bytes)
        }
    }

    deserializer.deserialize_str(ValueVisitor)
}
