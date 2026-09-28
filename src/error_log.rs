//! Complete upstream error evidence, private to the server. Never used as client output.
use regex::Regex;
use serde_json::{Value, json};
use std::{fs::{self, OpenOptions}, io::{self, Write}, path::PathBuf, sync::{Mutex, OnceLock}, time::{SystemTime, UNIX_EPOCH}};

const REDACTED: &str = "[REDACTED]";
const ROTATE_BYTES: u64 = 32 * 1024 * 1024;
const ROTATIONS: usize = 5;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn credential_field(name: &str) -> bool {
    let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
    matches!(name.as_str(), "authorization" | "proxyauthorization" | "cookie" | "setcookie" | "apikey" | "password" | "passwd" | "clientsecret" | "accesstoken" | "refreshtoken" | "idtoken" | "token" | "secret" | "salt" | "privatekey" | "signingkey" | "encryptionkey" | "bridgekey" | "credentials" | "xapikey" | "clientpassword" | "authtoken" | "apitoken" | "bearertoken")
}

fn redact_text(text: &str, secrets: &[&str]) -> String {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| vec![
        (Regex::new(r"(?s)-----BEGIN [^-]*PRIVATE KEY-----.*?-----END [^-]*PRIVATE KEY-----").unwrap(), REDACTED),
        (Regex::new(r"(?i)\b(bearer|basic)[ \t]+[a-z0-9._~+/=-]+").unwrap(), "$1 [REDACTED]"),
        (Regex::new(r"(?im)(\b(?:set-cookie|cookie)[ \t]*:[ \t]*)[^\r\n]+").unwrap(), "$1[REDACTED]"),
        (Regex::new(r#"(?i)((?:authorization|proxy-authorization|token|api[_ -]?key|access[_ -]?token|refresh[_ -]?token|id[_ -]?token|password|passwd|client[_ -]?secret|secret|salt|private[_ -]?key|signing[_ -]?key)["']?[ \t]*[=:][ \t]*)(?:"[^"]*"|'[^']*'|[^\s,;}\]]+)"#).unwrap(), "$1[REDACTED]"),
        (Regex::new(r"\b(?:sk|ghp|github_pat)[_-][A-Za-z0-9_-]{6,}").unwrap(), REDACTED),
        (Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+").unwrap(), REDACTED),
        (Regex::new(r"([a-zA-Z][a-zA-Z0-9+.-]*://)[^\s/@]+:[^\s/@]+@").unwrap(), "$1[REDACTED]@"),
    ]);
    let mut value = text.to_owned();
    let mut known: Vec<_> = secrets.iter().filter(|s| !s.is_empty()).copied().collect();
    known.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in known { value = value.replace(secret, REDACTED); }
    for (pattern, replacement) in patterns { value = pattern.replace_all(&value, *replacement).into_owned(); }
    value
}

/// Preserve all non-credential fields and complete strings, including unknown codes.
pub fn sanitize(value: &Value, secrets: &[&str]) -> Value {
    match value {
        Value::String(s) => Value::String(redact_text(s, secrets)),
        Value::Array(items) => Value::Array(items.iter().map(|v| sanitize(v, secrets)).collect()),
        Value::Object(fields) => Value::Object(fields.iter().map(|(key, value)| {
            let value = if credential_field(key) && !value.is_null() { json!(REDACTED) } else { sanitize(value, secrets) };
            (redact_text(key, secrets), value)
        }).collect()),
        _ => value.clone(),
    }
}

pub struct ErrorLog { path: PathBuf }
impl ErrorLog {
    pub fn new(path: PathBuf) -> Self { Self { path } }
    fn prepare(&self) -> io::Result<()> {
        let parent = self.path.parent().ok_or_else(|| io::Error::other("error log needs a parent directory"))?;
        fs::create_dir_all(parent)?;
        if fs::symlink_metadata(parent)?.file_type().is_symlink() { return Err(io::Error::other("error log directory must not be a symlink")); }
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?; }
        if let Ok(meta) = fs::symlink_metadata(&self.path) {
            if !meta.is_file() { return Err(io::Error::other("error log must be a regular file")); }
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?; }
        }
        Ok(())
    }
    pub fn check(&self) -> io::Result<()> {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.prepare()?;
        self.open().map(|_| ())
    }
    fn open(&self) -> io::Result<std::fs::File> {
        let mut options = OpenOptions::new(); options.append(true).create(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        options.open(&self.path)
    }
    fn record(&self, mut record: Value, secrets: &[&str]) -> io::Result<()> {
        record["recorded_at_unix_ms"] = json!(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64);
        let mut data = serde_json::to_vec(&sanitize(&record, secrets)).map_err(io::Error::other)?;
        data.push(b'\n');
        let _guard = WRITE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.prepare()?;
        if fs::metadata(&self.path).is_ok_and(|m| m.len() > 0 && m.len().saturating_add(data.len() as u64) > ROTATE_BYTES) {
            for i in (1..=ROTATIONS).rev() {
                let old = if i == 1 { self.path.clone() } else { self.path.with_extension(format!("jsonl.{}", i - 1)) };
                let new = self.path.with_extension(format!("jsonl.{i}"));
                match fs::rename(old, new) { Ok(()) => {}, Err(e) if e.kind() == io::ErrorKind::NotFound => {}, Err(e) => return Err(e) }
            }
        }
        let mut file = self.open()?; file.write_all(&data)?; file.flush()?; file.sync_data()
    }
    pub fn record_event(&self, request_id: &str, http_status: u16, event: &Value, secrets: &[&str]) -> io::Result<()> {
        let mut event = event.clone(); let mut omitted = Vec::new();
        if let Some(response) = event.get_mut("response").and_then(Value::as_object_mut)
            && response.remove("output").is_some() { omitted.push("/response/output"); }
        if let Some(fields) = event.as_object_mut() {
            for key in ["input", "messages", "tools", "instructions", "headers"] {
                if fields.remove(key).is_some() { omitted.push(match key { "input"=>"/input", "messages"=>"/messages", "tools"=>"/tools", "instructions"=>"/instructions", _=>"/headers" }); }
            }
        }
        self.record(json!({"request_id":request_id,"http_status":http_status,"kind":"upstream_event","upstream_event":event,"omitted_non_error_fields":omitted}),secrets)
    }
    pub fn record_http(&self, request_id: &str, http_status: u16, body: &str, secrets: &[&str]) -> io::Result<()> {
        let parsed = serde_json::from_str::<Value>(body).unwrap_or_else(|_| json!(body));
        let clean = sanitize(&parsed, secrets);
        let text = if parsed == clean { redact_text(body, secrets) } else if parsed.is_string() { clean.as_str().unwrap_or_default().to_owned() } else { clean.to_string() };
        self.record(json!({"request_id":request_id,"http_status":http_status,"kind":"upstream_http_error","upstream_body":clean,"upstream_body_text":text}), secrets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotates_complete_records_without_truncation() {
        let dir = std::env::temp_dir().join(format!("error-rotate-{}",uuid::Uuid::new_v4()));
        let path = dir.join("errors.jsonl"); let log = ErrorLog::new(path.clone());
        log.check().unwrap(); OpenOptions::new().write(true).open(&path).unwrap().set_len(ROTATE_BYTES).unwrap();
        log.record_http("req_rotate",400,"complete trailing error",&[]).unwrap();
        assert!(path.with_extension("jsonl.1").exists());
        assert!(fs::read_to_string(path).unwrap().contains("complete trailing error"));
        fs::remove_dir_all(dir).unwrap();
    }
}
