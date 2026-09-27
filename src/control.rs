//! 单个桥接进程统一准入、队列和监控；配置修改不能与正在运行的请求交错。
use crate::{
    admission::{Gate, Policy},
    auth::Context,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
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
    pub async fn enter(
        self: Arc<Self>,
        ctx: &Context,
        model: &str,
    ) -> Result<Option<Lease>, &'static str> {
        let gate = {
            let mut i = self.lock();
            if !ctx.excel
                || !i.saved.policy.permits_request(model, Some(&ctx.scope))
                || !i.saved.policy.accounts.permits(&ctx.account)
            {
                return Ok(None);
            }
            if !i.active.insert(ctx.request_id.clone()) {
                return Err("duplicate active request");
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
            });
            while i.records.len() > 1000 {
                i.records.pop_back();
            }
            i.gate.clone()
        };
        let mut lease = Lease {
            control: self.clone(),
            id: ctx.request_id.clone(),
            permit: None,
        };
        match gate.acquire().await {
            Ok(permit) => {
                lease.permit = Some(permit);
                self.update(&lease.id, |r| {
                    r.status = "running".into();
                    r.queue_ms = Some(now().saturating_sub(r.started_at_ms));
                });
                Ok(Some(lease))
            }
            Err(_) => {
                self.update(&lease.id, |r| r.status = "rejected_capacity".into());
                Err("Excel concurrency or queue limit reached")
            }
        }
    }
    fn update(&self, id: &str, f: impl FnOnce(&mut Record)) {
        if let Some(row) = self.lock().records.iter_mut().find(|r| r.request_id == id) {
            f(row)
        }
    }
}
pub struct Lease {
    control: Arc<Control>,
    id: String,
    permit: Option<OwnedSemaphorePermit>,
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
            expires: 1,
        };
        let first = c.clone().enter(&ctx, "model").await.unwrap().unwrap();
        assert!(matches!(c.save(p.clone(), Some(1)), Err((409, _))));
        let other = c.clone();
        let mut ctx2 = ctx.clone();
        ctx2.request_id = "r2".into();
        let waiting = tokio::spawn(async move { other.enter(&ctx2, "model").await });
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
