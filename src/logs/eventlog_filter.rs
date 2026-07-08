//! Pure agent-side Event Log filtering (spec 004 R6 filter knobs).
//!
//! Two filters, both fixture-tested on any platform and applied in
//! `eventlog_parse::process_entry`:
//!
//! - [`EventIdFilter`] — Winlogbeat's `event_id` syntax
//!   (`"4624,4625,4700-4800,-4735"`): comma list, `N` / `N-M` include, `-N` /
//!   `-N-M` exclude. If any includes are present an event must match an include
//!   AND no exclude; excludes alone mean "everything except". Compiling this to
//!   an `EvtSubscribe` XPath query is a v2 optimization (documented in the
//!   spec); v1 filters agent-side after render.
//! - provider exclusion — case-insensitive exact match on `Provider Name`.
//!
//! Consumed by the Windows pump; the parser applies it so filtered events still
//! return `Handled` (the bookmark advances past them).
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// A parsed `eventlog_event_ids` spec: inclusive include/exclude ranges.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct EventIdFilter {
    includes: Vec<(u32, u32)>,
    excludes: Vec<(u32, u32)>,
}

impl EventIdFilter {
    /// Parse the Winlogbeat `event_id` syntax. Whitespace around commas and
    /// tokens is tolerated; a non-empty invalid token is a hard error (never a
    /// silent skip — spec 004 R6).
    pub(crate) fn parse(spec: &str) -> Result<Self, String> {
        let mut includes = Vec::new();
        let mut excludes = Vec::new();
        for raw in spec.split(',') {
            let tok = raw.trim();
            if tok.is_empty() {
                continue;
            }
            match tok.strip_prefix('-') {
                Some(rest) => excludes.push(parse_range(rest.trim(), tok)?),
                None => includes.push(parse_range(tok, tok)?),
            }
        }
        Ok(Self { includes, excludes })
    }

    /// Whether `id` passes the filter (should be shipped).
    ///
    /// An exclude match always wins. With no includes, everything not excluded
    /// passes; with includes, `id` must fall in an include range.
    #[must_use]
    pub(crate) fn matches(&self, id: u32) -> bool {
        if self.excludes.iter().any(|&(lo, hi)| id >= lo && id <= hi) {
            return false;
        }
        if self.includes.is_empty() {
            return true;
        }
        self.includes.iter().any(|&(lo, hi)| id >= lo && id <= hi)
    }
}

/// Parse a `N` or `N-M` range body (the exclude `-` prefix is already stripped).
fn parse_range(body: &str, tok: &str) -> Result<(u32, u32), String> {
    match body.split_once('-') {
        Some((a, b)) => {
            let lo = parse_id(a, tok)?;
            let hi = parse_id(b, tok)?;
            if lo > hi {
                return Err(format!("invalid event id range '{tok}': {lo} > {hi}"));
            }
            Ok((lo, hi))
        }
        None => {
            let n = parse_id(body, tok)?;
            Ok((n, n))
        }
    }
}

fn parse_id(s: &str, tok: &str) -> Result<u32, String> {
    s.trim()
        .parse::<u32>()
        .map_err(|_| format!("invalid event id token '{tok}'"))
}

/// Combined agent-side filter: event-id ranges plus provider exclusion.
#[derive(Debug, Default, Clone)]
pub(crate) struct EventFilter {
    event_ids: Option<EventIdFilter>,
    exclude_providers: Vec<String>,
}

impl EventFilter {
    /// Build from the raw config knobs, parsing the event-id spec (a parse
    /// failure surfaces as a startup config error).
    pub(crate) fn new(
        event_ids: Option<&str>,
        exclude_providers: &[String],
    ) -> Result<Self, String> {
        let event_ids = match event_ids {
            Some(s) => Some(EventIdFilter::parse(s)?),
            None => None,
        };
        Ok(Self {
            event_ids,
            exclude_providers: exclude_providers.to_vec(),
        })
    }

    /// Whether an event must be filtered out (not shipped). The provider match
    /// is case-insensitive exact; the event-id filter is applied only when the
    /// id parses as a number (an unparseable id is never dropped by it).
    #[must_use]
    pub(crate) fn excludes(&self, provider: &str, event_id: &str) -> bool {
        if self
            .exclude_providers
            .iter()
            .any(|p| p.eq_ignore_ascii_case(provider))
        {
            return true;
        }
        if let Some(f) = &self.event_ids
            && let Ok(id) = event_id.parse::<u32>()
            && !f.matches(id)
        {
            return true;
        }
        false
    }
}
