//! Persisted, time-decayed access store.
//!
//! `AccessCounter` (see `access.rs`) is a per-node in-memory tally that drives
//! the BM25 access boost and resets on restart — that's the right behavior for
//! a session-local recency signal. `recent_hot`, though, wants the *opposite*:
//! a durable "which sections does this corpus actually rely on" signal that
//! survives restarts and doesn't let a section hammered months ago squat the
//! top slot forever. That's what this store provides.
//!
//! Each access decays the running score to *now*, then adds 1 — a standard
//! time-decayed accumulator. Entries are keyed by `(rel_path, heading_path)`,
//! not node id, so counts survive a reindex that renumbers nodes. The store is
//! pure (decay math + serde); the service owns reading/writing the sidecar
//! file, keeping library crates I/O-free.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Two-week half-life: a section not touched for two weeks counts half as
/// much as one touched today. Tunable; lives here so the decay unit is
/// documented in one place.
pub const DEFAULT_HALF_LIFE_SECS: f64 = 14.0 * 86_400.0;

/// One section's access history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRecord {
    pub rel_path: String,
    pub heading_path: Vec<String>,
    /// Total accesses ever — monotonic, never decayed. The "raw count".
    pub count: u32,
    /// Time-decayed accumulator as of `last_ts`. Read it through
    /// [`AccessStore::decayed`] to bring it to the current instant.
    pub score: f64,
    /// Unix seconds of the most recent access (the epoch `score` decays from).
    pub last_ts: u64,
}

/// A corpus's access history, keyed by `rel_path\u{1}heading` so a reindex
/// that renumbers nodes doesn't orphan the counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessStore {
    /// Not persisted — set from config after load so the on-disk file stays
    /// a plain entry map.
    #[serde(skip, default = "default_half_life")]
    half_life_secs: f64,
    entries: HashMap<String, AccessRecord>,
}

fn default_half_life() -> f64 {
    DEFAULT_HALF_LIFE_SECS
}

impl Default for AccessStore {
    fn default() -> Self {
        Self {
            half_life_secs: DEFAULT_HALF_LIFE_SECS,
            entries: HashMap::new(),
        }
    }
}

impl AccessStore {
    pub fn new(half_life_secs: f64) -> Self {
        Self {
            half_life_secs: half_life_secs.max(1.0),
            entries: HashMap::new(),
        }
    }

    /// Set the half-life after deserializing from disk (the field is skipped
    /// in the on-disk form).
    pub fn set_half_life(&mut self, secs: f64) {
        self.half_life_secs = secs.max(1.0);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn key(rel_path: &str, heading_path: &[String]) -> String {
        let mut k = String::with_capacity(rel_path.len() + 8);
        k.push_str(rel_path);
        k.push('\u{1}');
        for (i, seg) in heading_path.iter().enumerate() {
            if i > 0 {
                k.push('\u{1f}');
            }
            k.push_str(seg);
        }
        k
    }

    /// Decay `score` recorded at `last_ts` forward to `now`.
    fn decay(&self, score: f64, last_ts: u64, now: u64) -> f64 {
        let elapsed = now.saturating_sub(last_ts) as f64;
        score * 0.5_f64.powf(elapsed / self.half_life_secs)
    }

    /// Record an access: decay the existing score to `now`, add 1, bump the
    /// raw count.
    pub fn bump(&mut self, rel_path: &str, heading_path: &[String], now: u64) {
        // Capture the half-life before borrowing an entry mutably.
        let hl = self.half_life_secs;
        let key = Self::key(rel_path, heading_path);
        let rec = self.entries.entry(key).or_insert_with(|| AccessRecord {
            rel_path: rel_path.to_string(),
            heading_path: heading_path.to_vec(),
            count: 0,
            score: 0.0,
            last_ts: now,
        });
        let elapsed = now.saturating_sub(rec.last_ts) as f64;
        rec.score = rec.score * 0.5_f64.powf(elapsed / hl) + 1.0;
        rec.last_ts = now;
        rec.count = rec.count.saturating_add(1);
    }

    /// The decayed score for one section at `now`, or 0 if never accessed.
    pub fn decayed(&self, rel_path: &str, heading_path: &[String], now: u64) -> f64 {
        let key = Self::key(rel_path, heading_path);
        self.entries
            .get(&key)
            .map(|r| self.decay(r.score, r.last_ts, now))
            .unwrap_or(0.0)
    }

    /// Every record paired with its decayed score at `now`, sorted by decayed
    /// score descending. The service resolves each record back to a live node
    /// by `(rel_path, heading_path)`.
    pub fn ranked(&self, now: u64) -> Vec<(&AccessRecord, f64)> {
        let mut out: Vec<(&AccessRecord, f64)> = self
            .entries
            .values()
            .map(|r| (r, self.decay(r.score, r.last_ts, now)))
            .collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_increments_count_and_score() {
        let mut s = AccessStore::new(DEFAULT_HALF_LIFE_SECS);
        let h = vec!["A".to_string(), "B".to_string()];
        s.bump("a.md", &h, 1000);
        s.bump("a.md", &h, 1000);
        // Same instant: no decay, score == count == 2.
        assert_eq!(s.decayed("a.md", &h, 1000), 2.0);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn score_decays_over_time_but_count_does_not() {
        let hl = 100.0;
        let mut s = AccessStore::new(hl);
        let h = vec!["H".to_string()];
        s.bump("a.md", &h, 0);
        // One half-life later the decayed score is halved...
        assert!((s.decayed("a.md", &h, 100) - 0.5).abs() < 1e-9);
        // ...but the raw count is untouched.
        let rec = s.ranked(100)[0].0;
        assert_eq!(rec.count, 1);
    }

    #[test]
    fn recent_access_outranks_stale_heavier_one() {
        let hl = 100.0;
        let mut s = AccessStore::new(hl);
        let old = vec!["Old".to_string()];
        let fresh = vec!["Fresh".to_string()];
        // `old` accessed 5x long ago; `fresh` accessed once, recently.
        for _ in 0..5 {
            s.bump("a.md", &old, 0);
        }
        s.bump("b.md", &fresh, 1000); // 10 half-lives later
        let ranked = s.ranked(1000);
        assert_eq!(ranked[0].0.heading_path, fresh, "recent beats stale-heavy");
    }

    #[test]
    fn future_timestamp_does_not_amplify() {
        // Clock skew: an access "in the future" must not blow the score up.
        let mut s = AccessStore::new(100.0);
        let h = vec!["H".to_string()];
        s.bump("a.md", &h, 1000);
        // Reading at an earlier `now` saturates elapsed at 0 → no decay, not growth.
        assert_eq!(s.decayed("a.md", &h, 500), 1.0);
    }

    #[test]
    fn serde_round_trip_preserves_entries() {
        let mut s = AccessStore::new(DEFAULT_HALF_LIFE_SECS);
        let h = vec!["A".to_string()];
        s.bump("a.md", &h, 42);
        let json = serde_json::to_string(&s).unwrap();
        let mut back: AccessStore = serde_json::from_str(&json).unwrap();
        back.set_half_life(DEFAULT_HALF_LIFE_SECS);
        assert_eq!(back.len(), 1);
        assert_eq!(back.decayed("a.md", &h, 42), 1.0);
    }
}
