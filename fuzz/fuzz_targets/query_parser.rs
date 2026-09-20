#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(query) = std::str::from_utf8(data)
        && let Ok(parsed) = agenttraceback_search::SearchQuery::parse(query)
    {
        let _ = parsed.fts_match_expression();
        let _ = parsed.has_positive_text();
    }
});
