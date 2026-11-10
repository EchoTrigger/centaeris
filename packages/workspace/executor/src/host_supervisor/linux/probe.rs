//! Cancellation and bounded admission for read-only host probes.
use std::{
    os::fd::{AsRawFd, BorrowedFd},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, MutexGuard, TryLockError,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProbeBudget<'fd> {
    deadline: Instant,
    peer: Option<BorrowedFd<'fd>>,
}

impl<'fd> ProbeBudget<'fd> {
    pub(crate) fn new(deadline: Instant, peer: Option<BorrowedFd<'fd>>) -> Self {
        Self { deadline, peer }
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        loop {
            if Instant::now() >= self.deadline {
                return Err("host_probe_timeout".into());
            }
            let Some(peer) = self.peer else {
                return Ok(());
            };
            let mut descriptor = libc::pollfd {
                fd: peer.as_raw_fd(),
                // HUP/ERR/NVAL are always reported. RDHUP would also cancel a
                // peer that has finished its request but still awaits a response.
                events: 0,
                revents: 0,
            };
            // SAFETY: the borrowed descriptor remains live, and poll receives
            // one initialized output record for this nonblocking inspection.
            if unsafe { libc::poll(&mut descriptor, 1, 0) } < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err("host_probe_failed".into());
            }
            if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err("host_probe_cancelled".into());
            }
            return Ok(());
        }
    }

    pub(crate) fn remaining(&self) -> Result<Duration, String> {
        self.check()?;
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "host_probe_timeout".into())
    }

    pub(crate) fn lock<'mutex, T>(
        &self,
        mutex: &'mutex Mutex<T>,
    ) -> Result<MutexGuard<'mutex, T>, String> {
        loop {
            self.check()?;
            match mutex.try_lock() {
                Ok(guard) => {
                    // Waiting for this lock never creates a fresh deadline.
                    self.check()?;
                    return Ok(guard);
                }
                Err(TryLockError::Poisoned(_)) => return Err("host_probe_failed".into()),
                Err(TryLockError::WouldBlock) => {
                    thread::sleep(self.remaining()?.min(Duration::from_millis(10)));
                }
            }
        }
    }
}

/// Bound read-only work independently of task control.
#[derive(Debug)]
pub(super) struct ProbeAdmission {
    active: AtomicUsize,
}

impl ProbeAdmission {
    pub(super) const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
        }
    }

    pub(super) fn try_acquire(&self, budget: &ProbeBudget<'_>) -> Result<ProbePermit<'_>, String> {
        budget.check()?;
        // The host accepts eight clients; at most two perform probe work.
        const MAX_ACTIVE_PROBES: usize = 2;
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ACTIVE_PROBES).then_some(active + 1)
            })
            .map_err(|_| "host_probe_busy")?;
        let permit = ProbePermit(&self.active);
        budget.check()?;
        Ok(permit)
    }
}

#[derive(Debug)]
pub(super) struct ProbePermit<'admission>(&'admission AtomicUsize);

impl Drop for ProbePermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests;
