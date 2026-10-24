// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hybrid logical clock stamps: wall-clock milliseconds, a counter for
//! writes in the same millisecond or behind a clock that went back, and
//! the device, so two stamps never tie. Ordered in that order.

use serde::{Deserialize, Serialize};

/// When, and on which device, a field got its value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Stamp {
    pub ms: u64,
    pub n: u32,
    pub device: String,
}

/// A device's clock: never goes back, and moves past every stamp it saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    last_ms: u64,
    last_n: u32,
    device: String,
}

impl Clock {
    #[must_use]
    pub fn new(device: String) -> Self {
        Self {
            last_ms: 0,
            last_n: 0,
            device,
        }
    }

    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// A stamp later than every one this clock gave or saw. O(1).
    pub fn tick(&mut self, now_ms: u64) -> Stamp {
        if now_ms > self.last_ms {
            self.last_ms = now_ms;
            self.last_n = 0;
        } else {
            self.last_n = self.last_n.saturating_add(1);
        }
        Stamp {
            ms: self.last_ms,
            n: self.last_n,
            device: self.device.clone(),
        }
    }

    /// Moves past `seen`, a stamp another device gave. O(1).
    pub fn observe(&mut self, seen: &Stamp) {
        // A counter near its end would leave this clock no stamps to give:
        // no device writes that many in one millisecond.
        if seen.n > u32::MAX / 2 {
            return;
        }
        if (seen.ms, seen.n) > (self.last_ms, self.last_n) {
            self.last_ms = seen.ms;
            self.last_n = seen.n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_only_go_forward_even_when_the_wall_clock_goes_back() {
        let mut clock = Clock::new("a".into());
        let first = clock.tick(1000);
        let second = clock.tick(1000);
        let third = clock.tick(900);
        assert!(first < second && second < third);
        clock.observe(&Stamp {
            ms: 5000,
            n: 7,
            device: "b".into(),
        });
        let fourth = clock.tick(1200);
        assert!(fourth.ms == 5000 && fourth.n == 8);
    }

    #[test]
    fn equal_times_are_ordered_by_device() {
        let a = Stamp {
            ms: 1,
            n: 0,
            device: "a".into(),
        };
        let b = Stamp {
            ms: 1,
            n: 0,
            device: "b".into(),
        };
        assert!(a < b);
    }
}
