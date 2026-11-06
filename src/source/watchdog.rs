use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Time source. Tests supply a clock that does not move on its own.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Asks the driver to cancel the statement that is running.
pub trait Breaker: Send + Sync {
    fn interrupt(&self);
}

/// Wall clock used by production sessions.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// True once, when `now` is at or past `started + deadline` and nothing has fired yet.
pub(crate) fn should_fire(started: Instant, now: Instant, deadline: Duration, fired: bool) -> bool {
    !fired && now.saturating_duration_since(started) >= deadline
}

struct WatchState {
    started: Instant,
    deadline: Duration,
    fired: AtomicBool,
    stop: AtomicBool,
}

/// Background interrupt for a session deadline. Drop stops the thread and joins it.
pub struct Watchdog {
    state: Arc<WatchState>,
    clock: Arc<dyn Clock>,
    wake: Arc<(Mutex<()>, Condvar)>,
    join: Option<JoinHandle<()>>,
}

impl Watchdog {
    pub fn start(
        deadline: Duration,
        clock: Arc<dyn Clock>,
        breaker: Arc<dyn Breaker>,
        tick: Duration,
    ) -> Self {
        let state = Arc::new(WatchState {
            started: clock.now(),
            deadline,
            fired: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let wake = Arc::new((Mutex::new(()), Condvar::new()));
        let state_thread = Arc::clone(&state);
        let clock_thread = Arc::clone(&clock);
        let wake_thread = Arc::clone(&wake);
        let join = thread::spawn(move || {
            let (lock, condvar) = &*wake_thread;
            let mut guard = lock.lock().unwrap_or_else(|err| err.into_inner());
            while !state_thread.stop.load(Ordering::Acquire) {
                let fired = state_thread.fired.load(Ordering::Acquire);
                if should_fire(
                    state_thread.started,
                    clock_thread.now(),
                    state_thread.deadline,
                    fired,
                ) && state_thread
                    .fired
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    breaker.interrupt();
                }
                let (next, _) = condvar
                    .wait_timeout(guard, tick)
                    .unwrap_or_else(|err| err.into_inner());
                guard = next;
            }
        });
        Self {
            state,
            clock,
            wake,
            join: Some(join),
        }
    }

    pub fn expired(&self) -> bool {
        should_fire(
            self.state.started,
            self.clock.now(),
            self.state.deadline,
            false,
        )
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        self.wake.1.notify_all();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    struct ManualClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            }
        }

        fn advance(&self, by: Duration) {
            self.offset_ms.fetch_add(
                u64::try_from(by.as_millis()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    struct CountingBreaker {
        hits: AtomicUsize,
    }

    impl Breaker for CountingBreaker {
        fn interrupt(&self) {
            self.hits.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn should_fire_table() {
        let started = Instant::now();
        let cases = [
            (
                Duration::from_secs(0),
                Duration::from_secs(10),
                false,
                false,
            ),
            (
                Duration::from_secs(10),
                Duration::from_secs(10),
                false,
                true,
            ),
            (
                Duration::from_secs(11),
                Duration::from_secs(10),
                false,
                true,
            ),
            (
                Duration::from_secs(11),
                Duration::from_secs(10),
                true,
                false,
            ),
            (Duration::from_secs(0), Duration::ZERO, false, true),
        ];
        for (offset, deadline, fired, expect) in cases {
            assert_eq!(
                should_fire(started, started + offset, deadline, fired),
                expect,
                "offset={offset:?} deadline={deadline:?} fired={fired}"
            );
        }
    }

    #[test]
    fn does_not_interrupt_before_the_deadline() {
        let clock = Arc::new(ManualClock::new());
        let breaker = Arc::new(CountingBreaker {
            hits: AtomicUsize::new(0),
        });
        let watchdog = Watchdog::start(
            Duration::from_secs(60),
            clock,
            Arc::clone(&breaker) as Arc<dyn Breaker>,
            Duration::from_millis(10),
        );
        thread::sleep(Duration::from_millis(40));
        assert_eq!(breaker.hits.load(Ordering::SeqCst), 0);
        assert!(!watchdog.expired());
        drop(watchdog);
    }

    #[test]
    fn interrupts_once_after_the_deadline_and_join_on_drop() {
        let clock = Arc::new(ManualClock::new());
        let breaker = Arc::new(CountingBreaker {
            hits: AtomicUsize::new(0),
        });
        let watchdog = Watchdog::start(
            Duration::from_millis(30),
            Arc::clone(&clock) as Arc<dyn Clock>,
            Arc::clone(&breaker) as Arc<dyn Breaker>,
            Duration::from_millis(10),
        );
        clock.advance(Duration::from_millis(30));
        let started = Instant::now();
        while breaker.hits.load(Ordering::SeqCst) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "watchdog did not interrupt"
            );
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(40));
        assert_eq!(breaker.hits.load(Ordering::SeqCst), 1);
        assert!(watchdog.expired());
        drop(watchdog);
    }
}
