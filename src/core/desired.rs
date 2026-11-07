//! Desired vs. applied — the pure intent layer of the render pipeline.
//!
//! `DesiredWindow`/`DesiredState` describe where every managed window
//! *should* be after `layout::arrange` and `present::present_into`. Production
//! borrows the reusable `Placements` buffer directly: no X11 handles, no
//! `&State`, and no second per-frame heap copy.
//!
//! The reconciler owns the other half of the split: it diffs `DesiredState`
//! against `AppliedState` (what X11 actually has) and emits only the
//! `ConfigureWindow` deltas. The backend owns `AppliedState` and applies them.

use crate::types::{Rect, WindowId};

type Placement = (WindowId, Rect, u32);

/// One window's pure desired geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesiredWindow {
    pub window: WindowId,
    pub rect: Rect,
    pub border: u32,
}

/// Storage behind the public desired-window view.
///
/// Production uses `Borrowed`; `Owned` exists for test/model fixtures that
/// intentionally return or merge desired snapshots. Keeping both forms avoids
/// forcing test ownership concerns back into the production render path.
#[derive(Debug)]
pub enum DesiredWindows<'a> {
    Borrowed(&'a [Placement]),
    Owned(Vec<DesiredWindow>),
}

impl DesiredWindows<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::Borrowed(rows) => rows.len(),
            Self::Owned(rows) => rows.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> DesiredIter<'_> {
        match self {
            Self::Borrowed(rows) => DesiredIter::Borrowed(rows.iter()),
            Self::Owned(rows) => DesiredIter::Owned(rows.iter()),
        }
    }

    #[cfg(test)]
    pub fn extend(&mut self, other: DesiredWindows<'_>) {
        if !matches!(self, Self::Owned(_)) {
            let owned: Vec<_> = self.iter().collect();
            *self = Self::Owned(owned);
        }
        let Self::Owned(target) = self else {
            unreachable!("converted to owned desired storage")
        };
        target.extend(other.iter());
    }
}

pub enum DesiredIter<'a> {
    Borrowed(std::slice::Iter<'a, Placement>),
    Owned(std::slice::Iter<'a, DesiredWindow>),
}

impl Iterator for DesiredIter<'_> {
    type Item = DesiredWindow;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Borrowed(rows) => rows.next().map(|&(window, rect, border)| DesiredWindow {
                window,
                rect,
                border,
            }),
            Self::Owned(rows) => rows.next().copied(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Borrowed(rows) => rows.size_hint(),
            Self::Owned(rows) => rows.size_hint(),
        }
    }
}

impl DoubleEndedIterator for DesiredIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Borrowed(rows) => rows
                .next_back()
                .map(|&(window, rect, border)| DesiredWindow {
                    window,
                    rect,
                    border,
                }),
            Self::Owned(rows) => rows.next_back().copied(),
        }
    }
}

impl ExactSizeIterator for DesiredIter<'_> {}

impl<'a, 'b> IntoIterator for &'b DesiredWindows<'a> {
    type Item = DesiredWindow;
    type IntoIter = DesiredIter<'b>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Desired state for one monitor's arrange cycle.
#[derive(Debug)]
pub struct DesiredState<'a> {
    pub windows: DesiredWindows<'a>,
}

impl<'a> DesiredState<'a> {
    /// Production constructor: borrow the already-materialized placement buffer.
    pub fn from_placements(placements: &'a [Placement]) -> Self {
        Self {
            windows: DesiredWindows::Borrowed(placements),
        }
    }

    /// Owned fixture constructor. Production does not need this path.
    #[cfg(test)]
    pub fn from_windows(windows: Vec<DesiredWindow>) -> DesiredState<'static> {
        DesiredState {
            windows: DesiredWindows::Owned(windows),
        }
    }

    /// Transfer ownership of a placement vector into a desired snapshot.
    ///
    /// This is test-only because production already owns and reuses its
    /// `Placements` buffer; copying it into another owned representation would
    /// reintroduce the allocation this module is designed to avoid.
    #[cfg(test)]
    pub fn from_owned_placements(placements: Vec<Placement>) -> DesiredState<'static> {
        DesiredState::from_windows(
            placements
                .into_iter()
                .map(|(window, rect, border)| DesiredWindow {
                    window,
                    rect,
                    border,
                })
                .collect(),
        )
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn iter(&self) -> DesiredIter<'_> {
        self.windows.iter()
    }
}

#[cfg(test)]
impl Default for DesiredState<'static> {
    fn default() -> Self {
        Self {
            windows: DesiredWindows::Owned(Vec::new()),
        }
    }
}
