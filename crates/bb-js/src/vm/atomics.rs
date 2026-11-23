//! Atomics.wait / waitAsync / notify over Shared Data Blocks (§25.4.3.14 DoWait,
//! §25.4.15 Atomics.notify): WaiterLists live in the block, so agents on other threads
//! wake each other. A waitAsync promise is settled by its own agent, which polls its
//! pending waits from the job loop (`Vm::run_jobs`).

use super::typed::{SharedBlock, Signal, Waiter};
use super::value::Value;
use super::{JsResult, Vm};
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

/// A pending Atomics.waitAsync of this agent.
pub struct AsyncWait {
    pub block: Arc<SharedBlock>,
    pub signal: Arc<Signal>,
    pub deadline: Option<Instant>,
    pub resolve: Value,
}

pub(super) fn lock(b: &SharedBlock) -> MutexGuard<'_, Vec<Waiter>> {
    b.waiters.lock().unwrap_or_else(|e| e.into_inner())
}

fn notified(s: &Signal) -> bool {
    *s.notified.lock().unwrap_or_else(|e| e.into_inner())
}

/// Remove `signal`'s waiter if it's still listed: true when it was (it timed out).
fn remove_waiter(b: &SharedBlock, signal: &Arc<Signal>) -> bool {
    let mut w = lock(b);
    match w.iter().position(|x| Arc::ptr_eq(&x.signal, signal)) {
        Some(i) => {
            w.remove(i);
            true
        }
        None => false,
    }
}

/// The wait's deadline, `None` for +∞ (`t` in milliseconds, already clamped to ≥ 0).
pub(super) fn deadline(t: f64) -> Option<Instant> {
    if t.is_finite() {
        Some(Instant::now() + Duration::from_secs_f64(t / 1000.0))
    } else {
        None
    }
}

/// SuspendThisAgent: block until notified or the deadline; "ok" or "timed-out".
pub(super) fn wait_sync(b: &Arc<SharedBlock>, signal: &Arc<Signal>, until: Option<Instant>) -> &'static str {
    let mut n = signal.notified.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if *n {
            return "ok";
        }
        match until {
            None => n = signal.cv.wait(n).unwrap_or_else(|e| e.into_inner()),
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    drop(n);
                    // A notify that already removed us still counts.
                    return if remove_waiter(b, signal) { "timed-out" } else { "ok" };
                }
                n = signal.cv.wait_timeout(n, d - now).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }
}

/// RemoveWaiters + NotifyWaiter for up to `count` waiters on byte index `index`.
pub(super) fn notify(b: &SharedBlock, index: usize, count: f64) -> usize {
    let mut w = lock(b);
    let mut n = 0;
    let mut i = 0;
    while i < w.len() && (n as f64) < count {
        if w[i].index == index {
            let x = w.remove(i);
            *x.signal.notified.lock().unwrap_or_else(|e| e.into_inner()) = true;
            x.signal.cv.notify_all();
            n += 1;
        } else {
            i += 1;
        }
    }
    n
}

impl Vm {
    pub(super) fn add_async_wait(
        &mut self,
        block: Arc<SharedBlock>,
        signal: Arc<Signal>,
        deadline: Option<Instant>,
        resolve: Value,
    ) {
        self.async_waits.push(AsyncWait { block, signal, deadline, resolve });
    }

    /// Settle the waitAsync promises whose waiters were notified or timed out; true if any.
    pub(super) fn poll_async_waits(&mut self) -> JsResult<bool> {
        let mut any = false;
        let mut i = 0;
        while i < self.async_waits.len() {
            let w = &self.async_waits[i];
            let result = if notified(&w.signal) {
                Some("ok")
            } else if w.deadline.is_some_and(|d| Instant::now() >= d) {
                Some(if remove_waiter(&w.block, &w.signal) { "timed-out" } else { "ok" })
            } else {
                None
            };
            match result {
                Some(r) => {
                    let w = self.async_waits.remove(i);
                    self.call(&w.resolve, Value::Undefined, vec![Value::str(r)])?;
                    any = true;
                }
                None => i += 1,
            }
        }
        Ok(any)
    }

    /// Whether Atomics.waitAsync promises of this agent are still pending (hosts keep
    /// their event loop running for them).
    pub fn has_pending_waits(&self) -> bool {
        !self.async_waits.is_empty()
    }

    /// Sleep until a pending waitAsync can settle (notified or timed out) or `max` passes,
    /// then run the jobs that follow.
    pub fn wait_for_events(&mut self, max: Duration) {
        let until = Instant::now() + max;
        let next = self.async_waits.iter().filter_map(|w| w.deadline).min();
        let step = Duration::from_millis(1);
        while Instant::now() < until {
            if self.async_waits.iter().any(|w| notified(&w.signal)) {
                break;
            }
            if next.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            std::thread::sleep(step);
        }
        self.run_jobs();
    }
}
