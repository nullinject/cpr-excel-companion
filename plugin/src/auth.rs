//! 按账户保存的 Excel 通道授权。
//!
//! 复用 Excel 加载项自己的设备授权流程（`deviceauth`）：
//! 浏览器完成登录后得到一次性 `user_code`，再用 `device_auth_id + user_code` 换取 token。
//! 授权按 CPR 账户 ID 隔离，落盘在策略文件同目录。
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_plugin_sdk::{ErrorCode, PluginFault, call::host::HttpRequest, client::HostClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Excel 加载项（AppSource WA200010215）自己的 OAuth 客户端。
const CLIENT_ID: &str = "app_fnr0pYvVwwFDocDumLG3H2Bp";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const PENDING_TTL_SECONDS: u64 = 1800;
const MAX_CODE_BYTES: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    /// CPR 账户 ID（acct_…），授权按它绑定
    pub account_id: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub openai_account_id: String,
    #[serde(default)]
    pub openai_user_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// 会话来源：`excel-addin` 表示按加载项设备授权取得
    #[serde(default)]
    pub source: String,
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

fn header<'a>(headers: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| std::str::from_utf8(value).ok())
}

impl Auth {
    pub fn load(path: PathBuf) -> Result<Self, std::io::Error> {
        let store: Store = match fs::read(&path) {
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

    /// 向加载项的设备授权端点取一次跳转地址；`state` 即 `device_auth_id`。
    pub async fn start(&self, host: &HostClient, account_id: &str) -> Result<Value, PluginFault> {
        if account_id.is_empty() || account_id.len() > 128 {
            return Err(fault("invalid account id"));
        }
        let response = host
            .http(
                HttpRequest {
                    method: "GET".into(),
                    url: format!("{AUTHORIZE_URL}?client_id={CLIENT_ID}"),
                    headers: vec![("accept".into(), b"application/json".to_vec())],
                },
                Vec::new(),
            )
            .await?;
        let location = header(&response.headers, "location")
            .ok_or_else(|| fault("device authorization did not return a redirect"))?
            .to_owned();
        let device_auth_id = location
            .split_once("state=")
            .map(|(_, rest)| rest.split('&').next().unwrap_or_default())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| fault("device authorization has no state"))?
            .to_owned();
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain(|_, item| now().saturating_sub(item.created) < PENDING_TTL_SECONDS);
        pending.insert(
            device_auth_id.clone(),
            Pending {
                account_id: account_id.to_owned(),
                created: now(),
            },
        );
        Ok(json!({
            "accountId": account_id,
            "deviceAuthId": device_auth_id,
            "url": location,
            "expiresInSeconds": PENDING_TTL_SECONDS,
        }))
    }

    /// 用浏览器页面上的 `user_code` 换取 token。
    pub async fn complete(
        &self,
        host: &HostClient,
        device_auth_id: &str,
        user_code: &str,
    ) -> Result<Session, PluginFault> {
        let user_code = user_code.trim();
        if user_code.is_empty() || user_code.len() > MAX_CODE_BYTES {
            return Err(fault("invalid user code"));
        }
        let pending = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = pending
                .remove(device_auth_id)
                .ok_or_else(|| fault("unknown or expired device authorization"))?;
            if now().saturating_sub(entry.created) >= PENDING_TTL_SECONDS {
                return Err(fault("device authorization expired; start again"));
            }
            entry
        };
        let body = serde_json::to_vec(&json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
        }))
        .map_err(|_| fault("authorization encoding failed"))?;
        let response = host
            .http(
                HttpRequest {
                    method: "POST".into(),
                    url: TOKEN_URL.into(),
                    headers: vec![
                        ("content-type".into(), b"application/json".to_vec()),
                        ("accept".into(), b"application/json".to_vec()),
                    ],
                },
                body,
            )
            .await?;
        let status = response.status;
        let bytes = response.body.collect(65536).await?;
        if status != 200 {
            eprintln!("excel_auth: device token exchange rejected with status {status}");
            return Err(fault("authorization code was rejected"));
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
            scopes: Vec::new(),
            source: "excel-addin".to_owned(),
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
                    "source": session.source,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_device_auth_id_from_redirect() {
        let location = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_x\
&redirect_uri=https%3A%2F%2Fauth.openai.com%2Fbasispoints%2Fdeviceauth%2Fcallback\
&state=deviceauth-v2.abc123&code_challenge=xyz";
        let value = location
            .split_once("state=")
            .map(|(_, rest)| rest.split('&').next().unwrap_or_default());
        assert_eq!(value, Some("deviceauth-v2.abc123"));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let headers = vec![("Location".to_owned(), b"https://x".to_vec())];
        assert_eq!(header(&headers, "location"), Some("https://x"));
    }
}
