use super::eventlog_filter::{EventFilter, EventIdFilter};

// ─── EventIdFilter::parse + matches ──────────────────────────────────

fn parse(spec: &str) -> EventIdFilter {
    EventIdFilter::parse(spec).expect("valid spec")
}

#[test]
fn single_include_id() {
    let f = parse("4624");
    assert!(f.matches(4624));
    assert!(!f.matches(4625));
}

#[test]
fn include_list() {
    let f = parse("4624,4625,4634");
    assert!(f.matches(4624));
    assert!(f.matches(4625));
    assert!(f.matches(4634));
    assert!(!f.matches(4700));
}

#[test]
fn include_range() {
    let f = parse("4700-4800");
    assert!(f.matches(4700)); // inclusive lower
    assert!(f.matches(4750));
    assert!(f.matches(4800)); // inclusive upper
    assert!(!f.matches(4699));
    assert!(!f.matches(4801));
}

#[test]
fn exclude_only_means_everything_except() {
    let f = parse("-4735");
    assert!(!f.matches(4735));
    assert!(f.matches(4624), "no includes → everything else passes");
    assert!(f.matches(1));
}

#[test]
fn exclude_range() {
    let f = parse("-5152-5158");
    assert!(!f.matches(5152));
    assert!(!f.matches(5155));
    assert!(!f.matches(5158));
    assert!(f.matches(5159));
}

#[test]
fn include_and_exclude_full_winlogbeat_example() {
    // The canonical spec example: include 4624/4625/4700-4800, exclude 4735.
    let f = parse("4624,4625,4700-4800,-4735");
    assert!(f.matches(4624));
    assert!(f.matches(4625));
    assert!(f.matches(4700));
    assert!(f.matches(4800));
    // In the include range but explicitly excluded — exclude wins.
    assert!(!f.matches(4735));
    // Not in any include.
    assert!(!f.matches(9999));
}

#[test]
fn exclude_wins_over_include_same_id() {
    let f = parse("4624,-4624");
    assert!(!f.matches(4624), "exclude beats include for the same id");
}

#[test]
fn whitespace_tolerated() {
    let f = parse("  4624 , 4700 - 4800 , -4735  ");
    assert!(f.matches(4624));
    assert!(f.matches(4750));
    assert!(!f.matches(4735));
}

#[test]
fn empty_tokens_skipped() {
    let f = parse("4624,,4625,");
    assert!(f.matches(4624));
    assert!(f.matches(4625));
}

#[test]
fn empty_spec_matches_all() {
    let f = parse("");
    assert!(f.matches(1));
    assert!(f.matches(4624));
}

#[test]
fn invalid_token_is_error() {
    assert!(EventIdFilter::parse("4624,abc").is_err());
    assert!(EventIdFilter::parse("4624,").is_ok()); // trailing comma tolerated
    assert!(EventIdFilter::parse("-").is_err()); // bare minus, no number
    assert!(EventIdFilter::parse("12-").is_err()); // range missing upper
    assert!(EventIdFilter::parse("-x").is_err()); // exclude non-numeric
    assert!(EventIdFilter::parse("1-2-3").is_err()); // too many range parts
}

#[test]
fn reversed_range_is_error() {
    let err = EventIdFilter::parse("4800-4700").expect_err("reversed range invalid");
    assert!(err.contains("4800") && err.contains("4700"));
}

#[test]
fn negative_prefixed_range_excludes() {
    let f = parse("-4700-4800");
    assert!(!f.matches(4750));
    assert!(f.matches(4699));
}

// ─── EventFilter (provider exclusion + id filter) ────────────────────

#[test]
fn event_filter_default_excludes_nothing() {
    let f = EventFilter::default();
    assert!(!f.excludes("AnyProvider", "4624"));
}

#[test]
fn event_filter_provider_case_insensitive() {
    let f = EventFilter::new(None, &["Microsoft-Windows-Eventlog".to_string()]).unwrap();
    assert!(f.excludes("microsoft-windows-eventlog", "1"));
    assert!(f.excludes("MICROSOFT-WINDOWS-EVENTLOG", "1"));
    assert!(!f.excludes("Service Control Manager", "1"));
}

#[test]
fn event_filter_event_id_applied() {
    let f = EventFilter::new(Some("4624,4625"), &[]).unwrap();
    assert!(!f.excludes("X", "4624"), "included id kept");
    assert!(f.excludes("X", "4700"), "non-included id dropped");
}

#[test]
fn event_filter_unparseable_id_never_dropped_by_id_filter() {
    let f = EventFilter::new(Some("4624"), &[]).unwrap();
    // A non-numeric event_id can't be range-tested; the id filter leaves it.
    assert!(!f.excludes("X", "not-a-number"));
}

#[test]
fn event_filter_provider_and_id_combined() {
    let f = EventFilter::new(Some("4624"), &["Noisy".to_string()]).unwrap();
    assert!(f.excludes("Noisy", "4624"), "provider exclusion wins");
    assert!(f.excludes("Other", "9999"), "id filter drops non-included");
    assert!(!f.excludes("Other", "4624"));
}

#[test]
fn event_filter_new_propagates_parse_error() {
    assert!(EventFilter::new(Some("bogus"), &[]).is_err());
}
