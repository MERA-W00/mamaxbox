//! Approach after XCL d4c6d34: long title sleeps can overlap synchronous
//! pipeline, placement and submission work. Charge the union of those calls
//! once, even when several title threads enter the driver at the same time.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub fn enabled() -> bool {
    static ON: LazyLock<bool> =
        LazyLock::new(|| std::env::var_os("VICTORY_TITLE_SLEEP_HOLD").is_some_and(|v| v == "1"));
    *ON
}

#[derive(Default)]
struct Clock {
    calls: usize,
    last: Duration,
    frame_work: Duration,
    held: Duration,
}

impl Clock {
    fn advance(&mut self, now: Duration) {
        if self.calls != 0 {
            let elapsed = now - self.last;
            let grace = Duration::from_millis(250).saturating_sub(self.frame_work);
            self.held += elapsed.saturating_sub(grace);
            self.frame_work += elapsed;
        }
        self.last = now;
    }
}

static CLOCK: LazyLock<(Instant, Mutex<Clock>)> =
    LazyLock::new(|| (Instant::now(), Mutex::default()));

fn accounting<T>(f: impl FnOnce(&mut Clock) -> T) -> T {
    let mut c = CLOCK.1.lock().unwrap_or_else(|e| e.into_inner());
    c.advance(CLOCK.0.elapsed());
    f(&mut c)
}

pub(crate) struct Call;

pub(crate) fn begin() -> Option<Call> {
    enabled().then(|| {
        accounting(|c| c.calls += 1);
        Call
    })
}

impl Drop for Call {
    fn drop(&mut self) {
        accounting(|c| c.calls -= 1);
    }
}

pub(crate) fn presented() {
    if enabled() {
        accounting(|c| c.frame_work = Duration::ZERO);
    }
}

/// Total elapsed time during which long sleeps suspend their countdown.
pub fn elapsed() -> Duration {
    accounting(|c| c.held)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlapping_calls_share_a_grace_period_and_present_starts_another() {
        let mut c = Clock::default();
        let tick = Duration::from_millis;
        c.calls = 1;
        c.advance(tick(200));
        c.calls = 0;
        c.advance(tick(600));
        c.calls = 2;
        c.advance(tick(700));
        c.calls = 1;
        c.advance(tick(750));
        assert_eq!(c.held, tick(100));
        c.frame_work = Duration::ZERO;
        c.advance(tick(1001));
        assert_eq!(c.held, tick(101));
    }
}
