//! 签名上下文只由插件签发，不能接受客户端提供的账户或租户标记。
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub account: String,
    pub scope: String,
    pub request_id: String,
    pub excel: bool,
    /// 客户端 API key 的 sha256 摘要（request 阶段提取，随签名保护传输）；
    /// 桥接据此应用按 Key 的模型通道规则。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub expires: u64,
}

pub fn sign(context: &Context, secret: &[u8]) -> Result<String, &'static str> {
    if secret.len() < 32 {
        return Err("bridge secret requires at least 32 bytes");
    }
    let payload = serde_json::to_vec(context).map_err(|_| "invalid context")?;
    let encoded = URL_SAFE_NO_PAD.encode(payload);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| "invalid secret")?;
    mac.update(encoded.as_bytes());
    Ok(format!(
        "{encoded}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    ))
}

pub fn verify(token: &str, secret: &[u8], now: u64) -> Result<Context, &'static str> {
    if token.len() > 4096 || secret.len() < 32 {
        return Err("invalid bridge authorization");
    }
    let (payload, signature) = token.split_once('.').ok_or("missing signature")?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| "invalid signature")?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| "invalid secret")?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| "invalid signature")?;
    let data = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "invalid context")?;
    let context: Context = serde_json::from_slice(&data).map_err(|_| "invalid context")?;
    if context.expires <= now
        || context.expires > now.saturating_add(900)
        || context.account.is_empty()
        || context.account.len() > 256
        || context.scope.is_empty()
        || context.scope.len() > 256
        || context.request_id.is_empty()
        || context.request_id.len() > 256
    {
        return Err("expired or invalid bridge context");
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_context_is_bound_to_account_scope_mode_and_expiry() {
        let secret = b"synthetic-test-secret-at-least-32-bytes";
        let context = Context {
            account: "a".into(),
            scope: "tenant-a".into(),
            request_id: "req-test".into(),
            excel: true,
            key: None,
            expires: 150,
        };
        let token = sign(&context, secret).unwrap();
        assert_eq!(verify(&token, secret, 100).unwrap(), context);
        assert!(verify(&token, secret, 150).is_err());
        assert!(verify(&token, b"different-synthetic-secret-of-32-bytes", 100).is_err());
        let mut changed = context.clone();
        changed.account = "b".into();
        let altered = sign(&changed, secret).unwrap();
        let forged = format!(
            "{}.{}",
            altered.split_once('.').unwrap().0,
            token.split_once('.').unwrap().1
        );
        assert!(verify(&forged, secret, 100).is_err());
        changed.expires = 1001;
        assert!(verify(&sign(&changed, secret).unwrap(), secret, 100).is_err());
    }
}
