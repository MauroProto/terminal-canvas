pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    let query_chars: Vec<char> = query.chars().collect();
    let candidate_chars: Vec<char> = candidate.chars().collect();
    let candidate_lower: Vec<char> = candidate.to_lowercase().chars().collect();

    let mut score = 0;
    let mut qi = 0;
    let mut prev_matched = false;
    let mut consecutive = 0;
    let word_separators = [' ', '/', '-', '_', ':', '\\'];

    for (ci, &cc) in candidate_lower.iter().enumerate() {
        if qi < query_lower.len() && cc == query_lower[qi] {
            score += 1;

            if prev_matched {
                consecutive += 1;
                score += 5;
            } else {
                if qi > 0 {
                    score -= 1;
                }
                consecutive = 0;
            }

            if ci == 0 {
                score += 15;
            } else if word_separators.contains(&candidate_chars[ci - 1]) {
                score += 10;
            }

            if candidate_chars[ci] == query_chars[qi] {
                score += 1;
            }

            prev_matched = true;
            qi += 1;
        } else {
            prev_matched = false;
            consecutive = 0;
        }
    }

    if qi == query_lower.len() {
        let _ = consecutive;
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn consecutive_matches_score_higher() {
        let a = fuzzy_score("term", "New Terminal").unwrap();
        let b = fuzzy_score("tml", "New Terminal").unwrap();
        assert!(a > b);
    }

    #[test]
    fn missing_character_is_not_a_match() {
        assert_eq!(fuzzy_score("zzz", "New Terminal"), None);
    }
}
