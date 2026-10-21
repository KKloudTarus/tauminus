// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use thiserror::Error;

/// Terminal grid size in character cells.
///
/// Both dimensions are in `1..=TerminalSize::MAX_DIMENSION`. A zero dimension
/// breaks curses programs, and an unbounded one lets a bad resize request make
/// the emulator allocate a huge grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalSize {
    cols: u16,
    rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TerminalSizeError {
    #[error("terminal size {cols}x{rows} is outside 1..={max} cells per side")]
    OutOfRange { cols: u16, rows: u16, max: u16 },
}

impl TerminalSize {
    /// Upper bound per side. 4K displays with small fonts stay well under 1000
    /// columns; 2000 leaves headroom without allowing pathological grids.
    pub const MAX_DIMENSION: u16 = 2000;

    pub const fn new(cols: u16, rows: u16) -> Result<Self, TerminalSizeError> {
        if cols == 0 || rows == 0 || cols > Self::MAX_DIMENSION || rows > Self::MAX_DIMENSION {
            return Err(TerminalSizeError::OutOfRange {
                cols,
                rows,
                max: Self::MAX_DIMENSION,
            });
        }
        Ok(Self { cols, rows })
    }

    pub const fn cols(self) -> u16 {
        self.cols
    }

    pub const fn rows(self) -> u16 {
        self.rows
    }
}

impl Default for TerminalSize {
    /// The VT100 default of 80x24.
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_smallest_and_largest_sizes() {
        assert!(TerminalSize::new(1, 1).is_ok());
        let max = TerminalSize::MAX_DIMENSION;
        let size = TerminalSize::new(max, max).expect("max size is valid");
        assert_eq!((size.cols(), size.rows()), (max, max));
    }

    #[test]
    fn rejects_zero_columns_or_rows() {
        assert!(TerminalSize::new(0, 24).is_err());
        assert!(TerminalSize::new(80, 0).is_err());
    }

    #[test]
    fn rejects_dimension_above_max() {
        let over = TerminalSize::MAX_DIMENSION + 1;
        assert_eq!(
            TerminalSize::new(over, 24),
            Err(TerminalSizeError::OutOfRange {
                cols: over,
                rows: 24,
                max: TerminalSize::MAX_DIMENSION
            })
        );
        assert!(TerminalSize::new(80, u16::MAX).is_err());
    }

    #[test]
    fn default_is_vt100_size() {
        let size = TerminalSize::default();
        assert_eq!((size.cols(), size.rows()), (80, 24));
    }
}
