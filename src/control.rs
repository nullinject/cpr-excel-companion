//! 单个桥接进程统一准入、队列和监控；配置修改不能与正在运行的请求交错。
use crate::{
    admission::{Gate, Policy},
    auth::Context,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::OwnedSemaphorePermit;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
#[derive(Clone, Serialize)]
struct Record {
    request_id: String,
    model: String,
    client_key_id: String,
    account_id: String,
    status: String,
    started_at_ms: u64,
    queue_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    usage: Option<Value>,
    /// 桥接流内产生的错误说明；观察合并时保留首条。
    error: Option<String>,
    /// 宿主观察到的上游模型与终态错误码。
    upstream_model: Option<String>,
    error_code: Option<String>,
    /// 记录来源：bridge=桥接自查，host=仅宿主观察（请求未到桥接已失败等）。
    source: &'static str,
    /// 该请求最终是否走 Excel 通道。
    excel: bool,
    /// 签名请求携带的 key 摘要；用于观察归并时学习 hash → key ID。
    #[serde(skip_serializing)]
    key_hash: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    policy: Policy,
    version: u64,
}
struct Inner {
    saved: Saved,
    gate: Arc<Gate>,
    active: BTreeSet<String>,
    records: VecDeque<Record>,
    /// 学到的 key 摘要 → CPR key ID（来自观察归并），用于解析按 Key 规则。
    key_map: BTreeMap<String, String>,
}
pub struct Control {
    inner: Mutex<Inner>,
    path: PathBuf,
}
impl Control {
    pub fn load(path: PathBuf) -> Result<Self, &'static str> {
        let saved = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice::<Saved>(&b).map_err(|_| "invalid bridge policy")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Saved {
                policy: Policy::default(),
                version: 0,
            },
            Err(_) => return Err("policy file unavailable"),
        };
        let gate = Arc::new(Gate::new(&saved.policy)?);
        Ok(Self {
            inner: Mutex::new(Inner {
                saved,
                gate,
                active: BTreeSet::new(),
                records: VecDeque::new(),
                key_map: BTreeMap::new(),
            }),
            path,
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn snapshot(&self) -> Value {
        let i = self.lock();
        json!({"policy":i.saved.policy,"version":i.saved.version,"active":i.gate.active(),"waiting":i.gate.waiting(),"records":i.records})
    }
    pub fn save(
        &self,
        policy: Policy,
        expected: Option<u64>,
    ) -> Result<Value, (u16, &'static str)> {
        let gate = Arc::new(Gate::new(&policy).map_err(|e| (400, e))?);
        let mut i = self.lock();
        if !i.active.is_empty() {
            return Err((409, "请等待运行和排队请求结束后修改配置"));
        }
        if expected != Some(i.saved.version) {
            return Err((409, "配置已更新，请刷新页面"));
        }
        let saved = Saved {
            policy,
            version: i
                .saved
                .version
                .checked_add(1)
                .ok_or((500, "policy version overflow"))?,
        };
        let bytes = serde_json::to_vec(&saved).map_err(|_| (500, "policy encoding failed"))?;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let persist = || -> std::io::Result<()> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)
        };
        if persist().is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err((500, "policy persistence failed"));
        }
        i.saved = saved;
        i.gate = gate;
        Ok(json!({"version":i.saved.version}))
    }
    /// 签名请求的通道解析与准入：
    ///
    /// 1. model_channels 按基础模型名覆盖（Excel=无后缀强制走 Excel；Native=带后缀压回原生）；
    /// 2. 缺省跟随后缀语义（ctx.excel）；
    /// 3. accounts/models 粗粒度范围只约束 Excel 通道，原生透传不受并发预算约束。
    /// 4. 每个签名请求都留记录（含走原生的），侧边栏才能反映全量走向。
    pub async fn enter(
        self: &Arc<Self>,
        ctx: &Context,
        model: &str,
        suffix: &str,
        prewarm: bool,
    ) -> Lease {
        let base = model.strip_suffix(suffix).filter(|b| !b.is_empty()).unwrap_or(model);
        let (excel, gate, guard) = {
            let mut i = self.lock();
            let channel = i.saved.policy.model_channels.get(base).copied();
            // CPR 探针/预热（generate=false）不进 Excel 通道：Excel 无法预热，转原生。
            let desired = if prewarm {
                false
            } else {
                match channel {
                    Some(crate::admission::Channel::Excel) => true,
                    Some(crate::admission::Channel::Native) => false,
                    None => ctx.excel,
                }
            };
            let permitted = i.saved.policy.enabled
                && i.saved.policy.models.permits(model)
                && i.saved.policy.accounts.permits(&ctx.account);
            let excel = desired && permitted;
            if !i.active.insert(ctx.request_id.clone()) {
                return Lease {
                    control: self.clone(),
                    id: ctx.request_id.clone(),
                    _permit: None,
                    _active: None,
                    excel: false,
                    rejected: true,
                };
            }
            i.records.retain(|r| r.request_id != ctx.request_id);
            i.records.push_front(Record {
                request_id: ctx.request_id.clone(),
                model: model.into(),
                client_key_id: ctx.scope.clone(),
                account_id: ctx.account.clone(),
                status: "queued".into(),
                started_at_ms: now(),
                queue_ms: None,
                finished_at_ms: None,
                usage: None,
                error: None,
                upstream_model: None,
                error_code: None,
                source: "bridge",
                excel,
                key_hash: ctx.key.clone(),
            });
            while i.records.len() > 1000 {
                i.records.pop_back();
            }
            let guard = ActiveGuard {
                control: self.clone(),
                id: ctx.request_id.clone(),
            };
            (excel, i.gate.clone(), Some(guard))
        };
        let permit = if excel {
            match gate.acquire().await {
                Ok(permit) => {
                    self.update(&ctx.request_id, |r| {
                        r.status = "running".into();
                        r.queue_ms = Some(now().saturating_sub(r.started_at_ms));
                    });
                    Some(permit)
                }
                Err(_) => {
                    self.update(&ctx.request_id, |r| r.status = "rejected_capacity".into());
                    None
                }
            }
        } else {
            self.update(&ctx.request_id, |r| r.status = "running".into());
            None
        };
        let rejected = excel && permit.is_none();
        Lease {
            control: self.clone(),
            id: ctx.request_id.to_owned(),
            _permit: permit,
            _active: guard,
            excel,
            rejected,
        }
    }
    fn update(&self, id: &str, f: impl FnOnce(&mut Record)) {
        if let Some(row) = self.lock().records.iter_mut().find(|r| r.request_id == id) {
            f(row)
        }
    }
    /// 未签名透传请求的记录句柄；不占用 Excel 准入预算。
    pub fn enter_unsigned(
        self: &Arc<Self>,
        request_id: &str,
        model: &str,
        account_id: Option<&str>,
    ) -> Result<UnsignedLease, &'static str> {
        let mut i = self.lock();
        i.records.push_front(Record {
            request_id: request_id.to_owned(),
            model: model.to_owned(),
            client_key_id: "unsigned".into(),
            account_id: account_id.unwrap_or("unknown").to_owned(),
            status: "running".into(),
            started_at_ms: now(),
            queue_ms: None,
            finished_at_ms: None,
            usage: None,
            error: None,
            upstream_model: None,
            error_code: None,
            source: "unsigned",
            excel: false,
            key_hash: None,
        });
        while i.records.len() > 1000 {
            i.records.pop_back();
        }
        Ok(UnsignedLease {
            control: self.clone(),
            id: request_id.to_owned(),
        })
    }
    pub fn running(&self, request_id: &str, queue_ms: u64) {
        self.update(request_id, |r| {
            r.status = "running".into();
            r.queue_ms = Some(queue_ms);
        });
    }
    pub fn finish_unsigned(&self, request_id: &str, event: &Value) {
        self.update(request_id, |r| {
            r.status = event["response"]["status"]
                .as_str()
                .unwrap_or("failed")
                .into();
            r.usage = event["response"].get("usage").cloned();
            r.finished_at_ms = Some(now());
        });
    }
    /// 合并宿主最终观察：真实 Key 身份、上游模型、终态与错误码。
    /// 观察有界且不重投；未知请求只接受带 -excel 后缀的模型（宿主侧已失败、未到桥接）。
    pub fn observe(&self, observation: &crate::observe::ObserveEvent, suffix: &str) {
        let excel_requested = observation
            .requested_model
            .as_deref()
            .is_some_and(|m| m.ends_with(suffix) && m != suffix);
        let mut i = self.lock();
        if let Some(row) = i
            .records
            .iter_mut()
            .find(|r| r.request_id == observation.request_id)
        {
            row.client_key_id = observation
                .client_key_id
                .clone()
                .unwrap_or_else(|| row.client_key_id.clone());
            if let Some(account) = &observation.account_id {
                row.account_id = account.clone();
            }
            row.upstream_model = observation.upstream_model.clone();
            if let Some(usage) = &observation.usage {
                row.usage = serde_json::to_value(usage).ok();
            }
            if let Some(terminal) = &observation.terminal {
                row.status = format!("{:?}", terminal.outcome).to_lowercase();
                row.error_code = terminal.error_code.clone();
            }
            if let Some(failure) = &observation.failure {
                if failure.error_code.is_some() {
                    row.error_code = failure.error_code.clone();
                }
                if let Some(status) = failure.upstream_status_code {
                    row.error = Some(format!("upstream status {status}"));
                }
            }
            let learn = row
                .key_hash
                .as_ref()
                .zip(observation.client_key_id.as_ref())
                .map(|(hash, key_id)| (hash.clone(), key_id.clone()));
            row.finished_at_ms = Some(observation.completed_at_ms);
            if let Some((hash, key_id)) = learn {
                i.key_map.insert(hash, key_id);
            }
            return;
        }
        if !excel_requested || !i.saved.policy.enabled {
            return;
        }
        let status = observation
            .terminal
            .as_ref()
            .map(|t| format!("{:?}", t.outcome).to_lowercase())
            .unwrap_or_else(|| "unknown".into());
        i.records.push_front(Record {
            request_id: observation.request_id.clone(),
            model: observation.requested_model.clone().unwrap_or_default(),
            client_key_id: observation.client_key_id.clone().unwrap_or_default(),
            account_id: observation.account_id.clone().unwrap_or_default(),
            status,
            started_at_ms: observation.completed_at_ms,
            queue_ms: None,
            finished_at_ms: Some(observation.completed_at_ms),
            usage: observation
                .usage
                .as_ref()
                .and_then(|u| serde_json::to_value(u).ok()),
            error: observation
                .failure
                .as_ref()
                .and_then(|f| f.upstream_status_code.map(|s| format!("upstream status {s}"))),
            upstream_model: observation.upstream_model.clone(),
            excel: true,
            key_hash: None,
            error_code: observation
                .terminal
                .as_ref()
                .and_then(|t| t.error_code.clone())
                .or_else(|| {
                    observation
                        .failure
                        .as_ref()
                        .and_then(|f| f.error_code.clone())
                }),
            source: "host",
        });
        while i.records.len() > 1000 {
            i.records.pop_back();
        }
    }
}
/// active 集合的 Drop 守卫：排队请求被取消（future 中止）时也能清理。
struct ActiveGuard {
    control: Arc<Control>,
    id: String,
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.control.lock().active.remove(&self.id);
    }
}
pub struct Lease {
    control: Arc<Control>,
    id: String,
    _permit: Option<OwnedSemaphorePermit>,
    _active: Option<ActiveGuard>,
    /// 本次请求的最终通道；rejected = 需要 Excel 但并发/队列拒绝。
    pub excel: bool,
    pub rejected: bool,
}
impl Lease {
    pub fn terminal(&self, event: &Value) {
        self.control.update(&self.id, |r| {
            r.status = event["response"]["status"]
                .as_str()
                .unwrap_or("failed")
                .into();
            r.usage = event["response"].get("usage").cloned();
            r.finished_at_ms = Some(now());
        })
    }
    pub fn failed(&self) {
        self.control.update(&self.id, |r| {
            r.status = "failed".into();
            r.finished_at_ms = Some(now());
        })
    }
    /// 记录桥接侧失败原因；观察合并保留宿主终态，不覆盖已写的 error。
    pub fn fail(&self, message: &str) {
        self.control.update(&self.id, |r| {
            r.error = Some(message.to_owned());
            r.status = "failed".into();
            r.finished_at_ms = Some(now());
        })
    }
}
pub struct UnsignedLease {
    control: Arc<Control>,
    id: String,
}
impl UnsignedLease {
    pub fn fail(&self, message: &str) {
        self.control.update(&self.id, |r| {
            r.error = Some(message.to_owned());
            r.status = "failed".into();
            r.finished_at_ms = Some(now());
        })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut i = self.control.lock();
        i.active.remove(&self.id);
        if let Some(r) = i.records.iter_mut().find(|r| r.request_id == self.id)
            && r.finished_at_ms.is_none()
        {
            r.finished_at_ms = Some(now());
            if matches!(r.status.as_str(), "queued" | "running") {
                r.status = "cancelled".into();
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn policy_changes_wait_for_leases_and_cancelled_waiters_release_state() {
        let path = std::env::temp_dir().join(format!("excel-policy-{}", uuid::Uuid::new_v4()));
        let c = Arc::new(Control::load(path.clone()).unwrap());
        let p = Policy {
            enabled: true,
            ..Policy::default()
        };
        c.save(p.clone(), Some(0)).unwrap();
        let ctx = Context {
            account: "a".into(),
            scope: "k".into(),
            request_id: "r1".into(),
            excel: true,
            key: None,
            expires: 1,
        };
        let first = c.enter(&ctx, "model", "-excel", false).await;
        assert!(matches!(c.save(p.clone(), Some(1)), Err((409, _))));
        let mut ctx2 = ctx.clone();
        ctx2.request_id = "r2".into();
        let other = c.clone();
        let waiting = tokio::spawn(async move { other.enter(&ctx2, "model", "-excel", false).await });
        tokio::task::yield_now().await;
        assert_eq!(c.snapshot()["waiting"], 1);
        waiting.abort();
        let _ = waiting.await;
        assert_eq!(c.snapshot()["waiting"], 0);
        drop(first);
        assert_eq!(c.snapshot()["active"], 0);
        c.save(p, Some(1)).unwrap();
        assert_eq!(
            Control::load(path.clone()).unwrap().snapshot()["version"],
            2
        );
        std::fs::remove_file(path).unwrap();
    }
}
