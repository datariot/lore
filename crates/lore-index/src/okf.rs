//! Open Knowledge Format (OKF) frontmatter projection.
//!
//! [OKF](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)
//! is Google Cloud's vendor-neutral spec for packaging knowledge as markdown +
//! YAML frontmatter. Lore is a *consumer* of that format: when a document
//! carries OKF frontmatter, we surface its `type`, `status`, trust tier, and
//! author-declared staleness on query results. See
//! `docs/decisions/0005-okf-alignment.md`.
//!
//! Everything here is a pure projection over the decoded frontmatter
//! (`serde_json::Value`) — no derived index, safe to call at query time. A
//! document with no OKF frontmatter simply yields `None` for every field, so
//! ordinary Obsidian notes are unaffected.

use serde_json::Value;

/// Trust tier derived from an OKF `verified` list, per the spec's actor rules.
///
/// A document with no `verified` key is *unverified* and maps to `None` rather
/// than a variant here — most notes are unverified and we don't want to spend a
/// response field saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustTier {
    /// `verified` present, but every verifier is a non-human actor
    /// (`agent/version` or `process:id`).
    MachineConfirmed,
    /// At least one verifier is a `human:<id>` actor.
    HumanReviewed,
}

impl TrustTier {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustTier::MachineConfirmed => "machine-confirmed",
            TrustTier::HumanReviewed => "human-reviewed",
        }
    }
}

/// The frontmatter `type` — a free-form concept-kind string. OKF names this
/// field `type`, but so do many Obsidian vaults with their own vocabulary; we
/// can't tell them apart and deliberately surface whatever the author wrote
/// (e.g. `roadmap`, `moc`, `daily`). Only a non-empty top-level string counts;
/// a list or map value is ignored.
pub fn concept_type(fm: Option<&Value>) -> Option<&str> {
    string_field(fm, "type")
}

/// The frontmatter `status` — OKF's convention is `draft` | `stable` |
/// `deprecated`, but a vault may use any lifecycle vocabulary (`active`,
/// `archived`, …); we return whatever string the author wrote. Consumers treat
/// it as an opaque cue, not an enum.
pub fn status(fm: Option<&Value>) -> Option<&str> {
    string_field(fm, "status")
}

/// The raw OKF `stale_after` value — a `YYYY-MM-DD` date (or an ISO 8601
/// datetime whose date part we use). Returned verbatim; use
/// [`is_declared_stale`] for the comparison.
pub fn stale_after(fm: Option<&Value>) -> Option<&str> {
    string_field(fm, "stale_after")
}

/// Whether the author has declared this concept stale as of `now_unix`
/// (Unix epoch seconds). True once `now_unix` reaches midnight UTC of the
/// `stale_after` date. `false` when there is no `stale_after` or it doesn't
/// parse.
pub fn is_declared_stale(fm: Option<&Value>, now_unix: u64) -> bool {
    let Some((y, m, d)) = stale_after(fm).and_then(parse_ymd) else {
        return false;
    };
    let threshold = days_from_civil(y, m, d).saturating_mul(86_400);
    (now_unix as i64) >= threshold
}

/// The trust tier derived from the OKF `verified` frontmatter family. `None`
/// when `verified` is absent (unverified). Accepts either a list of
/// `{by, at}` maps or a single bare `{by, at}` map (the spec allows the
/// one-element shorthand).
pub fn trust_tier(fm: Option<&Value>) -> Option<TrustTier> {
    let verified = fm?.get("verified")?;
    let entries: Vec<&Value> = match verified {
        Value::Array(items) => items.iter().collect(),
        map @ Value::Object(_) => vec![map],
        _ => return None,
    };
    let mut any_verifier = false;
    for entry in entries {
        let Some(by) = entry.get("by").and_then(Value::as_str) else {
            continue;
        };
        any_verifier = true;
        if by.starts_with("human:") {
            return Some(TrustTier::HumanReviewed);
        }
    }
    any_verifier.then_some(TrustTier::MachineConfirmed)
}

fn string_field<'a>(fm: Option<&'a Value>, key: &str) -> Option<&'a str> {
    fm?.get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Parse the leading `YYYY-MM-DD` of a date or datetime string.
fn parse_ymd(s: &str) -> Option<(i64, i64, i64)> {
    let head = s.get(..10)?;
    let b = head.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = head[0..4].parse().ok()?;
    let m: i64 = head[5..7].parse().ok()?;
    let d: i64 = head[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// Days from the Unix epoch (1970-01-01) to the given proleptic-Gregorian date.
/// Howard Hinnant's `days_from_civil` algorithm — exact, branch-light, no deps.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn concept_type_and_status_project_strings() {
        let fm = json!({ "type": "BigQuery Table", "status": "stable" });
        assert_eq!(concept_type(Some(&fm)), Some("BigQuery Table"));
        assert_eq!(status(Some(&fm)), Some("stable"));
    }

    #[test]
    fn non_string_fields_are_ignored() {
        let fm = json!({ "type": ["a", "b"], "status": 3 });
        assert_eq!(concept_type(Some(&fm)), None);
        assert_eq!(status(Some(&fm)), None);
    }

    #[test]
    fn missing_frontmatter_yields_none() {
        assert_eq!(concept_type(None), None);
        assert_eq!(trust_tier(None), None);
        assert!(!is_declared_stale(None, 10_000_000_000));
    }

    #[test]
    fn epoch_civil_conversion_is_exact() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        // 2026-05-28 is 20601 days after the epoch.
        assert_eq!(days_from_civil(2026, 5, 28), 20601);
    }

    #[test]
    fn declared_stale_flips_on_the_date() {
        // stale_after 2026-05-28 → midnight UTC epoch = 20601 * 86400.
        let fm = json!({ "stale_after": "2026-05-28" });
        let midnight = 20601i64 * 86_400;
        assert!(!is_declared_stale(Some(&fm), (midnight - 1) as u64));
        assert!(is_declared_stale(Some(&fm), midnight as u64));
        assert!(is_declared_stale(Some(&fm), (midnight + 86_400) as u64));
    }

    #[test]
    fn declared_stale_accepts_datetime_form() {
        let fm = json!({ "stale_after": "2026-05-28T14:30:00Z" });
        let midnight = 20601i64 * 86_400;
        assert!(is_declared_stale(Some(&fm), midnight as u64));
    }

    #[test]
    fn bad_date_is_never_stale() {
        assert!(!is_declared_stale(
            Some(&json!({ "stale_after": "someday" })),
            u64::MAX
        ));
        assert!(!is_declared_stale(
            Some(&json!({ "stale_after": "2026-13-01" })),
            u64::MAX
        ));
    }

    #[test]
    fn trust_tier_from_list() {
        let human = json!({ "verified": [{ "by": "process:nightly" }, { "by": "human:dave" }] });
        assert_eq!(trust_tier(Some(&human)), Some(TrustTier::HumanReviewed));

        let machine = json!({ "verified": [{ "by": "reference_agent/gemini-2.5-pro" }] });
        assert_eq!(
            trust_tier(Some(&machine)),
            Some(TrustTier::MachineConfirmed)
        );
    }

    #[test]
    fn trust_tier_accepts_bare_map() {
        let bare = json!({ "verified": { "by": "human:ahormati", "at": "2026-05-01T00:00:00Z" } });
        assert_eq!(trust_tier(Some(&bare)), Some(TrustTier::HumanReviewed));
    }

    #[test]
    fn no_verified_key_is_unverified() {
        // `generated` alone does not confer trust — only `verified` does.
        let fm = json!({ "generated": { "by": "agent/x", "at": "2026-05-01T00:00:00Z" } });
        assert_eq!(trust_tier(Some(&fm)), None);
    }

    #[test]
    fn empty_verified_list_is_unverified() {
        assert_eq!(trust_tier(Some(&json!({ "verified": [] }))), None);
    }
}
