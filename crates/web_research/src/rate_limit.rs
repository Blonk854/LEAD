use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Per-host spacing for polite fetches shared across search/fetch paths.
///
/// Soft failures (`penalize`) stretch the delay up to `max_delay`; successes
/// (`reward`) reset the host back to the base delay.
#[derive(Clone)]
pub struct HostRateLimiter {
    inner: Arc<Mutex<HashMap<String, HostState>>>,
    delay: Duration,
    max_delay: Duration,
}

#[derive(Clone, Copy)]
struct HostState {
    last: Instant,
    strike: u32,
}

impl Default for HostRateLimiter {
    fn default() -> Self {
        Self::new(Duration::from_millis(1_000))
    }
}

impl HostRateLimiter {
    pub fn new(delay: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            delay,
            max_delay: delay.saturating_mul(8).max(delay),
        }
    }

    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay.max(self.delay);
        self
    }

    fn current_delay(&self, strike: u32) -> Duration {
        let mut delay = self.delay;
        for _ in 0..strike.min(4) {
            delay = delay.saturating_mul(2).min(self.max_delay);
        }
        delay
    }

    /// Block until this host may be contacted again.
    ///
    /// Intended to run on a background task (not the GPUI foreground thread).
    pub fn wait_turn_blocking(&self, host: &str) {
        let key = host.to_ascii_lowercase();
        loop {
            let sleep_for = {
                let mut map = self.inner.lock();
                let now = Instant::now();
                if let Some(state) = map.get(&key).copied() {
                    let delay = self.current_delay(state.strike);
                    if now < state.last + delay {
                        Some((state.last + delay) - now)
                    } else {
                        map.insert(
                            key.clone(),
                            HostState {
                                last: now,
                                strike: state.strike,
                            },
                        );
                        None
                    }
                } else {
                    map.insert(
                        key.clone(),
                        HostState {
                            last: now,
                            strike: 0,
                        },
                    );
                    None
                }
            };
            match sleep_for {
                Some(duration) => std::thread::sleep(duration),
                None => return,
            }
        }
    }

    /// Stretch spacing after soft blocks / thin pages / browser challenges.
    pub fn penalize(&self, host: &str) {
        let key = host.to_ascii_lowercase();
        let mut map = self.inner.lock();
        let state = map.entry(key).or_insert(HostState {
            last: Instant::now(),
            strike: 0,
        });
        state.strike = state.strike.saturating_add(1).min(4);
        state.last = Instant::now();
    }

    /// Reset adaptive backoff after a successful fetch.
    pub fn reward(&self, host: &str) {
        let key = host.to_ascii_lowercase();
        let mut map = self.inner.lock();
        if let Some(state) = map.get_mut(&key) {
            state.strike = 0;
            state.last = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_spacing_on_same_host() {
        let limiter = HostRateLimiter::new(Duration::from_millis(30));
        let start = Instant::now();
        limiter.wait_turn_blocking("example.com");
        limiter.wait_turn_blocking("example.com");
        assert!(start.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn penalize_increases_spacing() {
        let limiter = HostRateLimiter::new(Duration::from_millis(20)).with_max_delay(
            Duration::from_millis(200),
        );
        limiter.wait_turn_blocking("slow.test");
        limiter.penalize("slow.test");
        limiter.penalize("slow.test");
        let start = Instant::now();
        limiter.wait_turn_blocking("slow.test");
        // Two strikes → 20ms * 4 = 80ms base gap.
        assert!(start.elapsed() >= Duration::from_millis(60));
    }
}
