// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/// Byte thresholds for output flow control.
///
/// `high` pauses the producer and `low` resumes it. The gap between them stops
/// the session from flapping between paused and running on every batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watermarks {
    high: usize,
    low: usize,
}

impl Watermarks {
    /// Returns `None` unless `0 < low < high`.
    pub const fn new(high: usize, low: usize) -> Option<Self> {
        if low == 0 || low >= high {
            return None;
        }
        Some(Self { high, low })
    }

    pub const fn high(self) -> usize {
        self.high
    }

    pub const fn low(self) -> usize {
        self.low
    }
}

impl Default for Watermarks {
    /// 256 KiB high, 96 KiB low. The xterm.js flow control guide keeps
    /// unprocessed data under about 500 KB so keystrokes stay responsive; half
    /// of that bounds how long Ctrl-C waits behind a flood. The view
    /// acknowledges in batches smaller than `low`, so a paused session always
    /// resumes.
    fn default() -> Self {
        Self {
            high: 256 * 1024,
            low: 96 * 1024,
        }
    }
}

/// What the session pump must do after a flow control update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowSignal {
    Continue,
    Pause,
    Resume,
}

/// Tracks bytes sent to the terminal view that it has not yet acknowledged.
///
/// The view acknowledges bytes after the emulator has parsed them. While the
/// unacknowledged total is above the high watermark the pump stops reading
/// from the transport, which lets backpressure reach the remote process
/// through the PTY or SSH window instead of growing a buffer without bound.
#[derive(Debug)]
pub struct FlowControl {
    watermarks: Watermarks,
    in_flight: usize,
    paused: bool,
}

impl FlowControl {
    pub const fn new(watermarks: Watermarks) -> Self {
        Self {
            watermarks,
            in_flight: 0,
            paused: false,
        }
    }

    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    pub const fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Records `bytes` handed to the view.
    pub fn record_sent(&mut self, bytes: usize) -> FlowSignal {
        self.in_flight = self.in_flight.saturating_add(bytes);
        if !self.paused && self.in_flight > self.watermarks.high {
            self.paused = true;
            return FlowSignal::Pause;
        }
        FlowSignal::Continue
    }

    /// Records `bytes` the view has processed.
    ///
    /// An acknowledgement larger than the in-flight total is clamped to zero.
    /// The view is untrusted input, and a bad count must not wedge the
    /// session in the paused state.
    pub fn record_ack(&mut self, bytes: usize) -> FlowSignal {
        self.in_flight = self.in_flight.saturating_sub(bytes);
        if self.paused && self.in_flight < self.watermarks.low {
            self.paused = false;
            return FlowSignal::Resume;
        }
        FlowSignal::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> FlowControl {
        FlowControl::new(Watermarks::new(100, 20).expect("valid watermarks"))
    }

    #[test]
    fn watermarks_require_low_below_high_and_nonzero() {
        assert!(Watermarks::new(100, 20).is_some());
        assert!(Watermarks::new(100, 100).is_none());
        assert!(Watermarks::new(100, 200).is_none());
        assert!(Watermarks::new(100, 0).is_none());
    }

    /// The terminal view acknowledges in 64 KiB batches
    /// (`ACK_THRESHOLD_BYTES` in apps/desktop/src/features/terminal/ack-batcher.ts)
    /// and checks that number against 96 KiB. Changing `low` here means
    /// changing it there.
    #[test]
    fn default_watermarks_match_the_view_contract() {
        let w = Watermarks::default();
        assert_eq!((w.high(), w.low()), (256 * 1024, 96 * 1024));
    }

    #[test]
    fn default_watermarks_are_valid() {
        let w = Watermarks::default();
        assert!(Watermarks::new(w.high(), w.low()).is_some());
    }

    #[test]
    fn stays_running_at_exactly_high_watermark() {
        let mut flow = small();
        assert_eq!(flow.record_sent(100), FlowSignal::Continue);
        assert!(!flow.is_paused());
    }

    #[test]
    fn pauses_once_when_crossing_high_watermark() {
        let mut flow = small();
        assert_eq!(flow.record_sent(101), FlowSignal::Pause);
        assert_eq!(flow.record_sent(50), FlowSignal::Continue);
        assert!(flow.is_paused());
    }

    #[test]
    fn resumes_only_below_low_watermark() {
        let mut flow = small();
        flow.record_sent(150);
        assert_eq!(
            flow.record_ack(130),
            FlowSignal::Continue,
            "in flight = 20, not below low"
        );
        assert!(flow.is_paused());
        assert_eq!(flow.record_ack(1), FlowSignal::Resume);
        assert!(!flow.is_paused());
    }

    #[test]
    fn ack_while_running_never_signals_resume() {
        let mut flow = small();
        flow.record_sent(50);
        assert_eq!(flow.record_ack(50), FlowSignal::Continue);
    }

    #[test]
    fn over_ack_clamps_to_zero_and_resumes() {
        let mut flow = small();
        flow.record_sent(150);
        assert_eq!(flow.record_ack(usize::MAX), FlowSignal::Resume);
        assert_eq!(flow.in_flight(), 0);
    }

    #[test]
    fn sent_counter_saturates_instead_of_overflowing() {
        let mut flow = small();
        flow.record_sent(usize::MAX);
        flow.record_sent(usize::MAX);
        assert_eq!(flow.in_flight(), usize::MAX);
    }

    #[test]
    fn cycles_pause_and_resume_repeatedly() {
        let mut flow = small();
        for _ in 0..3 {
            assert_eq!(flow.record_sent(101), FlowSignal::Pause);
            assert_eq!(flow.record_ack(101), FlowSignal::Resume);
        }
    }
}
