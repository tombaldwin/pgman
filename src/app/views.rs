use super::*;

impl App {
    /// Compute the current per-transaction stats. Cheap —
    /// one pass over the ring.
    pub fn current_txns(&self) -> Vec<crate::tap::TxnStats> {
        crate::tap::group_by_txn(self.tap_events.iter())
    }

    /// Compute the current per-pool saturation stats. Cheap —
    /// one pass over the ring (plus a per-pool endpoint sweep
    /// for peak concurrency).
    pub fn current_pools(&self) -> Vec<crate::tap::PoolStats> {
        crate::tap::group_by_pool(self.tap_events.iter())
    }

    /// Compute the current baseline diff. Returns an empty
    /// vec when no baseline has been captured — the renderer
    /// detects that case and prompts the operator to press
    /// `Shift-B`.
    pub fn current_baseline_diff(&self) -> Vec<crate::tap::HotspotDiff> {
        let Some(baseline) = self.tap_baseline.as_ref() else {
            return Vec::new();
        };
        let current = self.current_hotspots();
        crate::tap::diff_hotspots(&baseline.hotspots, &current, false)
    }

    /// Compute the current hotspot list per `tap_sort`. Called
    /// each frame from the renderer and from the key handler.
    /// Cheap relative to the rest of the frame budget — ~2k
    /// events × one fingerprint each is sub-millisecond.
    pub fn current_hotspots(&self) -> Vec<crate::tap::Hotspot> {
        crate::tap::group_hotspots(self.tap_events.iter(), self.tap_nav.sort)
    }

    /// Compute the current N+1 findings — called by the panel
    /// renderer on demand. Uses the defaults
    /// (`NPLUS1_WINDOW_MICROS`, `NPLUS1_MIN_REPEATS`) which
    /// match the offline classifier's operating point.
    pub fn current_nplus1(&self) -> Vec<crate::tap::NplusOneFinding> {
        crate::tap::detect_nplus1(
            self.tap_events.iter(),
            crate::tap::NPLUS1_WINDOW_MICROS,
            crate::tap::NPLUS1_MIN_REPEATS,
        )
    }

    /// Compute the current per-caller rollup per `tap_sort`.
    /// Same shape as `current_hotspots` but the grouping key
    /// is the innermost caller frame instead of the SQL
    /// fingerprint.
    pub fn current_callers(&self) -> Vec<crate::tap::CallerStats> {
        crate::tap::group_by_caller(self.tap_events.iter(), self.tap_nav.sort)
    }
}
