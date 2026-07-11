//! The query: what the user redacted and wants to prove is gone.
//!
//! Mirrors Tumbler's search input model (spec §2, §6): a single word, a list
//! of words, or a regular expression, with case-sensitive and whole-word
//! toggles. The [`Query`] owns a compiled matcher so every extractor matches
//! identically — an extractor asks "does this decoded string contain the
//! term?" and gets back the matched span(s), never re-implementing the modes.

use regex::Regex;

/// How the query terms are interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QueryMode {
    /// Terms are literal substrings (subject to case/whole-word).
    Literal,
    /// The single term is a regular expression.
    Regex,
}

/// A single located match inside one decoded string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchSpan {
    /// Byte offset of the match start within the searched string.
    pub start: usize,
    /// Byte offset of the match end within the searched string.
    pub end: usize,
    /// The exact matched text.
    pub text: String,
}

/// A compiled query. Build with [`Query::literal`] or [`Query::regex`]; run
/// with [`Query::find_all`] / [`Query::is_match`].
#[derive(Clone, Debug)]
pub struct Query {
    terms: Vec<String>,
    mode: QueryMode,
    case_sensitive: bool,
    whole_word: bool,
    /// One regex per term (literal terms are escaped). Case-insensitivity and
    /// whole-word are baked into the pattern, so matching is a single code path.
    matchers: Vec<Regex>,
}

impl Query {
    /// A literal query over one or more terms.
    pub fn literal(
        terms: impl IntoIterator<Item = String>,
        case_sensitive: bool,
        whole_word: bool,
    ) -> Result<Self, String> {
        let terms: Vec<String> = terms.into_iter().filter(|t| !t.is_empty()).collect();
        if terms.is_empty() {
            return Err("no query terms provided".to_string());
        }
        let matchers = terms
            .iter()
            .map(|t| compile(&regex::escape(t), case_sensitive, whole_word))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            terms,
            mode: QueryMode::Literal,
            case_sensitive,
            whole_word,
            matchers,
        })
    }

    /// A regular-expression query (a single pattern).
    pub fn regex(pattern: String, case_sensitive: bool, whole_word: bool) -> Result<Self, String> {
        if pattern.is_empty() {
            return Err("empty regex pattern".to_string());
        }
        let matchers = vec![compile(&pattern, case_sensitive, whole_word)?];
        Ok(Self {
            terms: vec![pattern],
            mode: QueryMode::Regex,
            case_sensitive,
            whole_word,
            matchers,
        })
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }
    pub fn mode(&self) -> QueryMode {
        self.mode
    }
    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }
    pub fn whole_word(&self) -> bool {
        self.whole_word
    }

    /// True if any term matches anywhere in `haystack`.
    pub fn is_match(&self, haystack: &str) -> bool {
        self.matchers.iter().any(|re| re.is_match(haystack))
    }

    /// Every match of every term in `haystack`, in ascending start order.
    /// Overlapping matches across different terms are all reported (each term
    /// is a distinct thing the user asked about).
    pub fn find_all(&self, haystack: &str) -> Vec<MatchSpan> {
        let mut spans: Vec<MatchSpan> = self
            .matchers
            .iter()
            .flat_map(|re| {
                re.find_iter(haystack).map(|m| MatchSpan {
                    start: m.start(),
                    end: m.end(),
                    text: m.as_str().to_string(),
                })
            })
            .collect();
        spans.sort_by_key(|s| (s.start, s.end));
        spans
    }
}

/// Compiles one pattern with case/whole-word folded in. `(?i)` handles
/// case-insensitivity; `\b…\b` handles whole-word. The caller escapes literal
/// terms before calling.
fn compile(pattern: &str, case_sensitive: bool, whole_word: bool) -> Result<Regex, String> {
    let mut full = String::new();
    if !case_sensitive {
        full.push_str("(?i)");
    }
    if whole_word {
        full.push_str(r"\b(?:");
        full.push_str(pattern);
        full.push_str(r")\b");
    } else {
        full.push_str(pattern);
    }
    Regex::new(&full).map_err(|e| format!("invalid pattern '{pattern}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_case_insensitive_by_default() {
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        assert!(q.is_match("paid to zanzibar holdings"));
        assert!(q.is_match("ZANZIBAR"));
    }

    #[test]
    fn literal_case_sensitive_rejects_wrong_case() {
        let q = Query::literal(["Zanzibar".to_string()], true, false).unwrap();
        assert!(q.is_match("Zanzibar deal"));
        assert!(!q.is_match("zanzibar deal"));
    }

    #[test]
    fn whole_word_rejects_substring() {
        let q = Query::literal(["Zan".to_string()], false, true).unwrap();
        assert!(!q.is_match("Zanzibar"));
        assert!(q.is_match("the Zan file"));
    }

    #[test]
    fn literal_special_chars_are_escaped() {
        // A dot in a literal term must not act as a regex wildcard.
        let q = Query::literal(["a.c".to_string()], false, false).unwrap();
        assert!(q.is_match("a.c"));
        assert!(!q.is_match("abc"));
    }

    #[test]
    fn regex_matches_pattern() {
        let q = Query::regex(r"\d{3}-\d{2}-\d{4}".to_string(), false, false).unwrap();
        assert!(q.is_match("SSN 123-45-6789 here"));
        assert!(!q.is_match("no digits"));
    }

    #[test]
    fn invalid_regex_is_error() {
        assert!(Query::regex("[unterminated".to_string(), false, false).is_err());
    }

    #[test]
    fn empty_terms_is_error() {
        assert!(Query::literal(Vec::<String>::new(), false, false).is_err());
        assert!(Query::literal(["".to_string()], false, false).is_err());
    }

    #[test]
    fn find_all_reports_every_occurrence_sorted() {
        let q = Query::literal(["ab".to_string()], false, false).unwrap();
        let spans = q.find_all("ab_ab_ab");
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[1].start, 3);
        assert_eq!(spans[2].start, 6);
        assert!(spans.iter().all(|s| s.text == "ab"));
    }

    #[test]
    fn find_all_multi_term() {
        let q = Query::literal(["cat".to_string(), "dog".to_string()], false, false).unwrap();
        let spans = q.find_all("a dog and a cat");
        assert_eq!(spans.len(), 2);
        // Sorted by start: dog (2) before cat (12).
        assert_eq!(spans[0].text, "dog");
        assert_eq!(spans[1].text, "cat");
    }
}
