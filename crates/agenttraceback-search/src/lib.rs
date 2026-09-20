//! Structured query parsing, FTS5 expression generation, and stable result ranking.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use thiserror::Error;

/// Structured search query used by the local API and store.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchQuery {
    /// Positive and negative full-text terms.
    pub terms: Vec<SearchTerm>,
    /// Structured filters.
    pub filters: SearchFilters,
}

/// One full-text search term.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchTerm {
    /// Exact phrase or token text.
    pub text: String,
    /// Whether the term is excluded.
    pub negated: bool,
}

/// Structured filters parsed from the query syntax.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchFilters {
    /// Agent names.
    pub agents: Vec<FilterValue>,
    /// Model names.
    pub models: Vec<FilterValue>,
    /// Project names or paths.
    pub projects: Vec<FilterValue>,
    /// Normalized event actions.
    pub actions: Vec<FilterValue>,
    /// Path phrases.
    pub paths: Vec<FilterValue>,
    /// Risk severities.
    pub risks: Vec<FilterValue>,
    /// Evidence classes.
    pub evidence: Vec<FilterValue>,
    /// Result statuses.
    pub statuses: Vec<FilterValue>,
    /// Session UUIDs or prefixes.
    pub sessions: Vec<FilterValue>,
    /// Inclusive lower timestamp bound.
    pub after_us: Option<i64>,
    /// Exclusive upper timestamp bound.
    pub before_us: Option<i64>,
}

/// One structured filter value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterValue {
    /// Filter value.
    pub value: String,
    /// Whether the filter excludes matching rows.
    pub negated: bool,
}

/// Query parser failure with a byte position.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum QueryError {
    /// A quoted value was not terminated.
    #[error("unterminated quote at byte {position}")]
    UnterminatedQuote {
        /// Byte position of the opening quote.
        position: usize,
    },
    /// A filter lacked a value.
    #[error("filter `{name}` is missing a value")]
    MissingFilterValue {
        /// Filter name.
        name: String,
    },
    /// A filter name is not supported.
    #[error("unknown filter `{name}`")]
    UnknownFilter {
        /// Filter name.
        name: String,
    },
    /// A date filter was not an ISO calendar date.
    #[error("filter `{name}` requires a YYYY-MM-DD date")]
    InvalidDate {
        /// Filter name.
        name: String,
    },
}

impl SearchQuery {
    /// Parses structured filters and exact terms.
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        let tokens = tokenize(input)?;
        let mut query = Self::default();
        for token in tokens {
            let (negated, value) = token
                .strip_prefix('-')
                .map_or((false, token.as_str()), |value| (true, value));
            if let Some((name, filter_value)) = value.split_once(':') {
                apply_filter(&mut query.filters, name, filter_value, negated)?;
            } else {
                query.terms.push(SearchTerm {
                    text: value.to_owned(),
                    negated,
                });
            }
        }
        Ok(query)
    }

    /// Returns true when no text or filter would constrain the result set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.filters.is_empty()
    }

    /// Builds an FTS5 MATCH expression. Filters remain parameterized SQL.
    #[must_use]
    pub fn fts_match_expression(&self) -> String {
        self.terms
            .iter()
            .filter(|term| !term.negated)
            .map(|term| {
                let escaped = term.text.replace('"', "\"\"");
                format!("\"{escaped}\"")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// Returns only negative full-text terms for indexed exclusion clauses.
    pub fn negative_terms(&self) -> impl Iterator<Item = &SearchTerm> {
        self.terms.iter().filter(|term| term.negated)
    }
}

impl SearchFilters {
    /// Returns true when every vector filter is empty and no date bound exists.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
            && self.models.is_empty()
            && self.projects.is_empty()
            && self.actions.is_empty()
            && self.paths.is_empty()
            && self.risks.is_empty()
            && self.evidence.is_empty()
            && self.statuses.is_empty()
            && self.sessions.is_empty()
            && self.after_us.is_none()
            && self.before_us.is_none()
    }
}

fn tokenize(input: &str) -> Result<Vec<String>, QueryError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote_start = None;
    let mut quoted = false;

    for (position, character) in input.char_indices() {
        match character {
            '"' => {
                quoted = !quoted;
                quote_start = quoted.then_some(position);
                if !quoted && !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    if quoted {
        return Err(QueryError::UnterminatedQuote {
            position: quote_start.unwrap_or_default(),
        });
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn apply_filter(
    filters: &mut SearchFilters,
    name: &str,
    value: &str,
    negated: bool,
) -> Result<(), QueryError> {
    if value.is_empty() {
        return Err(QueryError::MissingFilterValue {
            name: name.to_owned(),
        });
    }
    match name {
        "agent" => filters.agents.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "model" => filters.models.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "project" => filters.projects.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "type" => filters.actions.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "path" => filters.paths.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "risk" => filters.risks.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "evidence" => filters.evidence.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "status" => filters.statuses.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "session" => filters.sessions.push(FilterValue {
            value: value.to_owned(),
            negated,
        }),
        "after" if !negated => filters.after_us = Some(parse_date(name, value, false)?),
        "before" if !negated => filters.before_us = Some(parse_date(name, value, true)?),
        _ => {
            return Err(QueryError::UnknownFilter {
                name: name.to_owned(),
            });
        }
    }
    Ok(())
}

fn parse_date(name: &str, value: &str, end_of_day: bool) -> Result<i64, QueryError> {
    let date =
        NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| QueryError::InvalidDate {
            name: name.to_owned(),
        })?;
    let date = if end_of_day {
        date.and_hms_opt(23, 59, 59)
            .ok_or_else(|| QueryError::InvalidDate {
                name: name.to_owned(),
            })?
    } else {
        date.and_hms_opt(0, 0, 0)
            .ok_or_else(|| QueryError::InvalidDate {
                name: name.to_owned(),
            })?
    };
    let datetime: DateTime<Utc> = Utc.from_utc_datetime(&date);
    datetime
        .timestamp_micros()
        .checked_add(if end_of_day { 999_999 } else { 0 })
        .ok_or_else(|| QueryError::InvalidDate {
            name: name.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::{QueryError, SearchQuery};

    #[test]
    fn parses_mixed_terms_and_quoted_filters() {
        let query = SearchQuery::parse(
            "agent:hermes model:\"deepseek-v4.1-flash\" path:\"src/auth.ts\" failed",
        )
        .expect("query");
        assert_eq!(query.filters.agents[0].value, "hermes");
        assert_eq!(query.filters.models[0].value, "deepseek-v4.1-flash");
        assert_eq!(query.filters.paths[0].value, "src/auth.ts");
        assert_eq!(query.terms[0].text, "failed");
    }

    #[test]
    fn parses_date_bounds() {
        let query = SearchQuery::parse("after:2026-09-01 before:2026-10-01").expect("query");
        assert!(query.filters.after_us.is_some());
        assert!(query.filters.before_us.is_some());
        assert!(query.filters.after_us < query.filters.before_us);
    }

    #[test]
    fn rejects_unknown_filter_and_unterminated_quote() {
        assert!(matches!(
            SearchQuery::parse("unknown:value"),
            Err(QueryError::UnknownFilter { .. })
        ));
        assert!(matches!(
            SearchQuery::parse("path:\"broken"),
            Err(QueryError::UnterminatedQuote { .. })
        ));
    }

    #[test]
    fn generates_quoted_fts_expression() {
        let query = SearchQuery::parse("oauth migration").expect("query");
        assert_eq!(query.fts_match_expression(), "\"oauth\" AND \"migration\"");
    }

    #[test]
    fn parses_negated_terms_and_filters() {
        let query = SearchQuery::parse("-agent:codex -draft").expect("query");
        assert!(query.filters.agents[0].negated);
        assert!(query.terms[0].negated);
    }
}
