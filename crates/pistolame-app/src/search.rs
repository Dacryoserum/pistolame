//! Library search: every word of the query must appear, ignoring case and accents
//! ("beyonce" finds "Beyoncé", "ost season 2" finds "… Season 2 (Original … OST)").

/// Lowercase, accents folded, for comparisons.
pub(crate) fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(fold_accent)
        .collect()
}

/// Whether every word of `query` occurs in one of `fields`.
pub(crate) fn matches(query_words: &[String], fields: &[&str]) -> bool {
    let haystack = normalize(&fields.join(" "));
    query_words
        .iter()
        .all(|word| haystack.contains(word.as_str()))
}

/// The normalized words of a query; empty for a blank query.
pub(crate) fn query_words(query: &str) -> Vec<String> {
    normalize(query)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Folds the accented Latin letters found in French, Spanish, German and Nordic titles.
fn fold_accent(character: char) -> char {
    match character {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'œ' => 'o',
        'æ' => 'a',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_must_match_ignoring_case_and_accents() {
        let words = query_words("beyonce LEMONADE");
        assert!(matches(&words, &["Lemonade", "Beyoncé"]));
        assert!(!matches(&words, &["Lemonade", "Rihanna"]));
        assert!(matches(
            &query_words("season 2"),
            &["Game of Thrones: Season 2"]
        ));
        assert!(query_words("   ").is_empty());
    }
}
