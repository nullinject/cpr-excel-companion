//! 按账户保存的 Excel/BPS 授权：Codex OAuth（PKCE）换取 token，落盘在策略文件同目录。
//!
//! 授权按 CPR 账户 ID 隔离；未授权的账户继续使用宿主注入的凭据，行为不变。
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_plugin_sdk::{ErrorCode, PluginFault, call::host::HttpRequest, client::HostClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// 与 Codex Desktop 相同的 loopback 回调；授权结束后浏览器地址栏即回调地址。
pub const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access api.connectors.read api.connectors.invoke";
const PENDING_TTL_SECONDS: u64 = 600;
const MAX_CALLBACK_BYTES: usize = 8192;

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    /// CPR 账户 ID（acct_…），授权按它绑定
    pub account_id: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// OpenAI 账号 ID（来自 id_token），用于覆盖账号身份头
    #[serde(default)]
    pub openai_account_id: String,
    #[serde(default)]
    pub openai_user_id: String,
    /// Unix 秒；0 表示上游未返回有效期
    #[serde(default)]
    pub expires_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    accounts: BTreeMap<String, Session>,
}

struct Pending {
    verifier: String,
    account_id: String,
    created: u64,
}

pub struct Auth {
    path: PathBuf,
    store: Mutex<Store>,
    pending: Mutex<BTreeMap<String, Pending>>,
}

fn fault(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, message)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn random_token() -> String {
    // 两个 UUIDv4 提供约 244 位随机性，足够作为 PKCE verifier 与 state。
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn jwt_claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

fn claim(claims: &Value, name: &str) -> String {
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get(name))
        .or_else(|| claims.get(name))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl Auth {
    pub fn load(path: PathBuf) -> Result<Self, std::io::Error> {
        let store = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            store: Mutex::new(store),
            pending: Mutex::new(BTreeMap::new()),
        })
    }

    fn save(&self, store: &Store) -> Result<(), PluginFault> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| fault("authorization file needs a parent directory"))?;
        fs::create_dir_all(parent).map_err(|_| fault("authorization directory is not writable"))?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(store).map_err(|_| fault("authorization encoding failed"))?,
        )
        .map_err(|_| fault("authorization file is not writable"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&temporary, &self.path)
            .map_err(|_| fault("authorization file cannot be replaced"))?;
        Ok(())
    }

    /// 为账户生成授权链接；state 同时用于回调校验与一次性关联。
    pub fn start(&self, account_id: &str) -> Result<Value, PluginFault> {
        if account_id.is_empty() || account_id.len() > 128 {
            return Err(fault("invalid account id"));
        }
        let verifier = random_token();
        let state = random_token();
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain(|_, item| now().saturating_sub(item.created) < PENDING_TTL_SECONDS);
        pending.insert(
            state.clone(),
            Pending {
                verifier: verifier.clone(),
                account_id: account_id.to_owned(),
                created: now(),
            },
        );
        let query = [
            ("client_id", CLIENT_ID.to_owned()),
            ("response_type", "code".to_owned()),
            ("redirect_uri", REDIRECT_URI.to_owned()),
            ("scope", SCOPE.to_owned()),
            ("state", state.clone()),
            ("code_challenge_method", "S256".to_owned()),
            ("code_challenge", challenge(&verifier)),
            ("prompt", "login".to_owned()),
        ];
        let encoded: Vec<String> = query
            .iter()
            .map(|(name, value)| format!("{}={}", name, url_encode(value)))
            .collect();
        Ok(json!({
            "accountId": account_id,
            "state": state,
            "redirectUri": REDIRECT_URI,
            "url": format!("{AUTHORIZE_URL}?{}", encoded.join("&")),
            "expiresInSeconds": PENDING_TTL_SECONDS,
        }))
    }

    /// 用回调地址（或纯 code）完成换取；成功后按账户保存。
    pub async fn complete(
        &self,
        host: &HostClient,
        state: &str,
        callback: &str,
    ) -> Result<Session, PluginFault> {
        if callback.len() > MAX_CALLBACK_BYTES {
            return Err(fault("callback is too large"));
        }
        let pending = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = pending
                .remove(state)
                .ok_or_else(|| fault("unknown or expired state"))?;
            if now().saturating_sub(entry.created) >= PENDING_TTL_SECONDS {
                return Err(fault("authorization expired; start again"));
            }
            entry
        };
        let code =
            extract_code(callback).ok_or_else(|| fault("callback has no authorization code"))?;
        let body = [
            ("grant_type", "authorization_code".to_owned()),
            ("client_id", CLIENT_ID.to_owned()),
            ("code", code),
            ("code_verifier", pending.verifier.clone()),
            ("redirect_uri", REDIRECT_URI.to_owned()),
        ]
        .iter()
        .map(|(name, value)| format!("{}={}", name, url_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
        let response = host
            .http(
                HttpRequest {
                    method: "POST".into(),
                    url: TOKEN_URL.into(),
                    headers: vec![
                        (
                            "content-type".into(),
                            b"application/x-www-form-urlencoded".to_vec(),
                        ),
                        ("accept".into(), b"application/json".to_vec()),
                    ],
                },
                body.into_bytes(),
            )
            .await?;
        let status = response.status;
        let bytes = response.body.collect(65536).await?;
        if status != 200 {
            eprintln!("excel_auth: token exchange rejected with status {status}");
            return Err(fault("authorization was rejected by the identity service"));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| fault("invalid token response"))?;
        let access_token = value["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| fault("token response has no access token"))?
            .to_owned();
        let claims = jwt_claims(&access_token);
        let expires_at = value["expires_in"]
            .as_u64()
            .map(|seconds| now().saturating_add(seconds))
            .unwrap_or_default();
        let session = Session {
            account_id: pending.account_id,
            access_token,
            refresh_token: value["refresh_token"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            openai_account_id: claim(&claims, "chatgpt_account_id"),
            openai_user_id: claim(&claims, "chatgpt_account_user_id"),
            expires_at,
        };
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store
            .accounts
            .insert(session.account_id.clone(), session.clone());
        self.save(&store)?;
        Ok(session)
    }

    pub fn status(&self) -> Value {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let accounts: Vec<Value> = store
            .accounts
            .values()
            .map(|session| {
                json!({
                    "accountId": session.account_id,
                    "expiresAt": session.expires_at,
                    "expired": session.expires_at != 0 && session.expires_at <= now(),
                    "openaiAccountId": session.openai_account_id,
                })
            })
            .collect();
        json!({ "accounts": accounts, "file": self.path.to_string_lossy() })
    }

    pub fn logout(&self, account_id: &str) -> Result<(), PluginFault> {
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if store.accounts.remove(account_id).is_some() {
            self.save(&store)?;
        }
        Ok(())
    }

    /// 账户已授权且未过期时返回可用于覆盖宿主凭据的会话。
    pub fn session_for(&self, account_id: &str) -> Option<Session> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = store.accounts.get(account_id)?;
        if session.expires_at != 0 && session.expires_at <= now() {
            return None;
        }
        Some(session.clone())
    }
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 接受完整回调地址、仅查询串或裸 code。
fn extract_code(callback: &str) -> Option<String> {
    let query = callback
        .split_once('?')
        .map(|(_, rest)| rest)
        .unwrap_or(callback);
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == "code" && !value.is_empty() {
            return Some(percent_decode(value));
        }
    }
    let trimmed = callback.trim();
    if !trimmed.is_empty() && !trimmed.contains(['?', '&', '=']) {
        return Some(trimmed.to_owned());
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_code_from_callback_shapes() {
        assert_eq!(
            extract_code("http://127.0.0.1:1455/auth/callback?code=abc123&state=x").as_deref(),
            Some("abc123")
        );
        assert_eq!(extract_code("code=abc%2Fdef").as_deref(), Some("abc/def"));
        assert_eq!(extract_code("  abc123  ").as_deref(), Some("abc123"));
        assert!(extract_code("http://127.0.0.1:1455/auth/callback?error=denied").is_none());
    }

    #[test]
    fn pkce_challenge_is_stable() {
        let verifier = "test-verifier";
        assert_eq!(challenge(verifier), challenge(verifier));
        assert_ne!(challenge(verifier), challenge("other"));
    }
}
