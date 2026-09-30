//! 网关准入规则与有界 FIFO 等待。许可必须随响应流持有至结束或取消。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    #[serde(default)]
    pub allow: BTreeSet<String>,
    #[serde(default)]
    pub deny: BTreeSet<String>,
}
impl Scope {
    pub fn permits(&self, value: &str) -> bool {
        !self.deny.contains(value) && (self.allow.is_empty() || self.allow.contains(value))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Overflow {
    Queue,
    Reject,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Excel,
    #[default]
    Native,
}
/// 按基础模型名的通道覆盖；缺省跟随后缀语义。
/// Excel = 无后缀请求也强制走 Excel（仅签名请求）；Native = 带 -excel 后缀也压回原生。
pub type ModelChannels = BTreeMap<String, Channel>;

/// 已认证 Client Key 的模型通道覆盖；只选择通道，不授予模型或账户权限。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRule {
    #[serde(default)]
    pub models: ModelChannels,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub models: Scope,
    pub accounts: Scope,
    #[serde(default)]
    pub model_channels: ModelChannels,
    /// 按 CPR 已认证的 Client Key ID 和基础模型选择通道。
    #[serde(default)]
    pub key_rules: BTreeMap<String, KeyRule>,
    pub concurrency: usize,
    pub overflow: Overflow,
    pub queue_capacity: usize,
    pub queue_timeout_ms: u64,
    /// Excel-only error classification; retry decisions remain with the caller.
    #[serde(default)]
    pub policy_errors_as_server_error: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            models: Scope::default(),
            accounts: Scope::default(),
            model_channels: BTreeMap::new(),
            key_rules: BTreeMap::new(),
            concurrency: 1,
            overflow: Overflow::Queue,
            queue_capacity: 32,
            queue_timeout_ms: 120_000,
            policy_errors_as_server_error: false,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=1024).contains(&self.concurrency) {
            return Err("concurrency must be between 1 and 1024");
        }
        if self.queue_capacity > 10_000 {
            return Err("queue_capacity must not exceed 10000");
        }
        if !(1..=600_000).contains(&self.queue_timeout_ms) {
            return Err("queue_timeout_ms must be between 1 and 600000");
        }
        if self.model_channels.len() > 200
            || self
                .model_channels
                .keys()
                .any(|model| !valid_identifier(model))
        {
            return Err("model_channels supports at most 200 models of 1 to 256 trimmed bytes");
        }
        if self.key_rules.len() > 256
            || self.key_rules.iter().any(|(key, rule)| {
                !valid_identifier(key)
                    || rule.models.len() > 200
                    || rule.models.keys().any(|model| !valid_identifier(model))
            })
        {
            return Err("key_rules supports at most 256 keys of 200 trimmed models each");
        }
        for scope in [&self.models, &self.accounts] {
            if scope.allow.len() + scope.deny.len() > 200
                || scope
                    .allow
                    .iter()
                    .chain(&scope.deny)
                    .any(|value| !valid_identifier(value))
            {
                return Err(
                    "each scope supports at most 200 identifiers of 1 to 256 trimmed bytes",
                );
            }
        }
        Ok(())
    }
    /// Routing precedence: authenticated Key/model, global model, requested channel.
    /// Admission and authorization are separate and cannot be bypassed by an override.
    pub fn channel_for(&self, model: &str, client_key_id: &str, requested: Channel) -> Channel {
        self.key_rules
            .get(client_key_id)
            .and_then(|rule| rule.models.get(model))
            .or_else(|| self.model_channels.get(model))
            .copied()
            .unwrap_or(requested)
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.trim() == value
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Busy,
    QueueFull,
    Timeout,
    Closed,
}

pub struct Gate {
    semaphore: Arc<Semaphore>,
    waiting: AtomicUsize,
    limit: usize,
    overflow: Overflow,
    queue_capacity: usize,
    timeout: Duration,
}
struct Waiting<'a>(&'a AtomicUsize);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Gate {
    pub fn new(policy: &Policy) -> Result<Self, &'static str> {
        policy.validate()?;
        Ok(Self {
            semaphore: Arc::new(Semaphore::new(policy.concurrency)),
            waiting: AtomicUsize::new(0),
            limit: policy.concurrency,
            overflow: policy.overflow,
            queue_capacity: policy.queue_capacity,
            timeout: Duration::from_millis(policy.queue_timeout_ms),
        })
    }
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::Acquire)
    }
    pub fn active(&self) -> usize {
        self.limit - self.semaphore.available_permits()
    }
    pub fn close(&self) {
        self.semaphore.close();
    }
    pub async fn acquire(&self) -> Result<OwnedSemaphorePermit, AdmissionError> {
        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => return Ok(permit),
            Err(tokio::sync::TryAcquireError::Closed) => return Err(AdmissionError::Closed),
            Err(tokio::sync::TryAcquireError::NoPermits) => {}
        }
        if self.overflow == Overflow::Reject {
            return Err(AdmissionError::Busy);
        }
        self.waiting
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.queue_capacity).then_some(n + 1)
            })
            .map_err(|_| AdmissionError::QueueFull)?;
        let _waiting = Waiting(&self.waiting);
        // Tokio Semaphore 按等待顺序分配；取消等待会自动移除对应申请。
        tokio::time::timeout(self.timeout, self.semaphore.clone().acquire_owned())
            .await
            .map_err(|_| AdmissionError::Timeout)?
            .map_err(|_| AdmissionError::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_fail_closed_and_deny_wins() {
        let mut scope = Scope::default();
        scope.allow.insert("allowed".into());
        assert!(scope.permits("allowed"));
        assert!(!scope.permits("other"));
        scope.deny.insert("allowed".into());
        assert!(!scope.permits("allowed"));
    }
    #[tokio::test]
    async fn reject_never_exceeds_limit() {
        let gate = Gate::new(&Policy {
            overflow: Overflow::Reject,
            ..Policy::default()
        })
        .unwrap();
        let first = gate.acquire().await.unwrap();
        assert!(matches!(gate.acquire().await, Err(AdmissionError::Busy)));
        assert_eq!(gate.active(), 1);
        drop(first);
        assert!(gate.acquire().await.is_ok());
    }
    #[tokio::test]
    async fn fifo_bounded_queue_and_cancellation() {
        let gate = Arc::new(
            Gate::new(&Policy {
                queue_capacity: 2,
                ..Policy::default()
            })
            .unwrap(),
        );
        let first = gate.acquire().await.unwrap();
        let g = gate.clone();
        let second = tokio::spawn(async move { g.acquire().await.unwrap() });
        tokio::task::yield_now().await;
        let g = gate.clone();
        let third = tokio::spawn(async move { g.acquire().await.unwrap() });
        tokio::task::yield_now().await;
        assert_eq!(gate.waiting(), 2);
        assert!(matches!(
            gate.acquire().await,
            Err(AdmissionError::QueueFull)
        ));
        drop(first);
        let second_permit = second.await.unwrap();
        assert!(!third.is_finished());
        third.abort();
        assert!(third.await.unwrap_err().is_cancelled());
        assert_eq!(gate.waiting(), 0);
        drop(second_permit);
        assert_eq!(gate.active(), 0);
        assert!(gate.acquire().await.is_ok());
    }
    #[tokio::test]
    async fn timeout_and_close_release_waiters() {
        let gate = Gate::new(&Policy {
            queue_timeout_ms: 1,
            ..Policy::default()
        })
        .unwrap();
        let _first = gate.acquire().await.unwrap();
        assert!(matches!(gate.acquire().await, Err(AdmissionError::Timeout)));
        assert_eq!(gate.waiting(), 0);
        gate.close();
        assert!(matches!(gate.acquire().await, Err(AdmissionError::Closed)));
    }
}
