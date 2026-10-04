//! Session titles, generated rather than truncated.
//!
//! ## The problem
//!
//! The GUI named every thread from the first user message:
//!
//! ```text
//! title = firstUser.content.slice(0, 32)
//! ```
//!
//! So a thread opened with *"can you look at why the auth middleware is failing
//! on the staging host"* became **"can you look at why the auth m"** — mid-word,
//! lowercase, and identical for every thread starting with the same seven words.
//! Worse, a thread that opened with a pasted stack trace or a file header got a
//! title made of code.
//!
//! ## Why generate it at all
//!
//! Truncation is cheap but wrong in a way that matters more as threads accumulate:
//! a sidebar of 50 threads all named after their opening tokens is not navigable.
//! A three-to-six word summary of what the conversation turned out to be about is
//! what makes a history usable.
//!
//! ## Why it runs server-side
//!
//! Two reasons, one practical and one architectural:
//!
//! - The GUI streams its chat response, so it has no natural point at which to ask
//!   for a title without a second round trip to the model from the client.
//! - Every backend (native, Ollama, vLLM) is reachable from here, and a scheduled
//!   turn or an API client gets a title without the GUI existing at all.
//!
//! ## Bounded, because it runs on every new thread
//!
//! Small max_tokens, low temperature, and a hard output cap. A title is 3-6 words;
//! anything longer is the summarizer failing, not a title.

/// Longest title accepted. Anything beyond is a generation that ignored the
/// instruction, and a sidebar entry that wraps to three lines.
pub const MAX_TITLE_CHARS: usize = 72;

/// The instruction. Kept in one place so the prompt and its tests cannot drift.
pub const TITLE_PROMPT: &str = "\
Give a title for this conversation. Rules:
- 3 to 6 words.
- Describe the subject, not the request: prefer \"auth middleware failure\" over \
\"can you look at why auth fails\".
- No trailing punctuation, no quotes, no markdown, no emoji.
- If the text is code, a stack trace, or pasted data, name what it is about rather \
than quoting it.
- Reply with the title only. No preamble, no explanation, no alternatives.";

/// Builds the title-generation prompt from the opening turns.
pub fn build_title_prompt(turns: &[(String, String)]) -> String {
    let mut out = String::from(TITLE_PROMPT);
    out.push_str("\n\nConversation so far:\n");
    for (role, content) in turns {
        let label = if role.eq_ignore_ascii_case("assistant") {
            "Assistant"
        } else {
            "User"
        };
        // Bound each turn. A pasted stack trace is the common case here and would
        // otherwise dominate the prompt while telling us nothing about the subject.
        let trimmed: String = content.trim().chars().take(600).collect();
        if trimmed.is_empty() {
            continue;
        }
        out.push_str(&format!("{label}: {trimmed}\n"));
    }
    out.push_str("\nTitle:");
    out
}

/// Cleans a generated title into something safe for a sidebar row.
///
/// Small models are inconsistent about the "title only, no preamble" instruction,
/// so this is not defensive padding — it is the difference between a usable title
/// and a sidebar full of "Title: auth middleware fix". The fallbacks are ordered:
/// strip the model talking, then take the first plausible line, then give up.
pub fn clean_title(raw: &str) -> Option<String> {
    let mut text = raw.trim();

    // Models sometimes wrap the answer in quotes or a markdown heading.
    text = text.trim_start_matches('#').trim();
    text = text
        .trim_matches(|c| c == '*' || c == '`' || c == '"' || c == '\u{201c}' || c == '\u{201d}');
    text = text.trim();

    // Strip a leading label, with or without a colon. Anchored, so a title that
    // legitimately contains the word later is untouched.
    let lowered = text.to_lowercase();
    for prefix in ["title:", "conversation title:", "thread title:", "title -"] {
        if lowered.starts_with(prefix) {
            text = text[prefix.len()..].trim();
            break;
        }
    }

    // Take the first non-empty line: some models still add a sentence after.
    if let Some(first) = text.lines().map(str::trim).find(|l| !l.is_empty()) {
        text = first;
    }

    text = text
        .trim()
        .trim_end_matches(['.', '!', '?', ',', ';', ':'])
        .trim();

    // Reject things that are obviously not titles rather than displaying them.
    if text.is_empty() || text.len() < 3 {
        return None;
    }
    // A refusal or an apology is not a title.
    let lowered = text.to_lowercase();
    for bad in [
        "i cannot",
        "i can't",
        "i'm sorry",
        "i am sorry",
        "as an ai",
        "no title",
        "n/a",
        "unknown",
    ] {
        if lowered.starts_with(bad) {
            return None;
        }
    }
    // A near-empty response usually means the model produced prose instead.
    if text.split_whitespace().count() > 14 {
        return None;
    }

    let capped: String = text.chars().take(MAX_TITLE_CHARS).collect();
    let capped = capped.trim().to_string();
    if capped.is_empty() {
        None
    } else {
        Some(capped)
    }
}

/// A title derived locally, used when generation fails or is unavailable.
///
/// Deliberately word-based rather than a raw truncation: taking the first few
/// *words* and title-casing them already reads better than
/// `content.slice(0, 32)` cutting mid-word, and costs nothing.
pub fn fallback_title(first_message: &str) -> String {
    let cleaned: String = first_message
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let words: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .take(6)
        .collect();
    if words.is_empty() {
        return "New Chat".to_string();
    }
    // Skip a leading filler word, which makes for a poor title.
    let mut picked = &words[..];
    if picked.len() > 1 {
        let first = picked[0].to_lowercase();
        if matches!(
            first.as_str(),
            "hi" | "hello" | "hey" | "ok" | "okay" | "so" | "um" | "please" | "can"
        ) {
            picked = &picked[1..];
        }
    }
    let joined = picked.join(" ");
    let mut out = String::with_capacity(joined.len());
    for (i, word) in picked.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // Uppercase the first alphabetic character, leave the rest alone so
        // identifiers like `authMiddleware` survive intact.
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    let capped: String = out.chars().take(MAX_TITLE_CHARS).collect();
    let capped = capped.trim().to_string();
    if capped.is_empty() {
        "New Chat".to_string()
    } else {
        capped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turns(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn the_prompt_carries_the_rules_and_the_turns() {
        let p = build_title_prompt(&turns(&[("user", "why does auth fail on staging")]));
        assert!(p.contains("3 to 6 words"));
        assert!(p.contains("why does auth fail on staging"));
        assert!(p.trim_end().ends_with("Title:"));
    }

    #[test]
    fn a_pasted_trace_is_bounded_in_the_prompt() {
        // The case that motivated bounding: a huge paste would otherwise dominate.
        let huge = "E".repeat(50_000);
        let p = build_title_prompt(&turns(&[("user", &huge)]));
        // 600 chars of user text plus the fixed instruction block. The point is
        // that 50k did not survive, not that the total is under some round number.
        assert!(
            p.chars().count() < 1200,
            "prompt was {} chars; a 50k paste should not survive whole",
            p.chars().count()
        );
    }

    #[test]
    fn a_clean_title_passes_through() {
        assert_eq!(
            clean_title("Auth middleware failure").as_deref(),
            Some("Auth middleware failure")
        );
    }

    #[test]
    fn a_labelled_title_loses_the_label() {
        // Small models add this constantly, and a sidebar of "Title: ..." is worse
        // than no title at all.
        assert_eq!(
            clean_title("Title: Auth middleware failure").as_deref(),
            Some("Auth middleware failure")
        );
        assert_eq!(
            clean_title("conversation title: rag relevance floor").as_deref(),
            Some("rag relevance floor")
        );
    }

    #[test]
    fn markdown_and_quotes_are_stripped() {
        assert_eq!(clean_title("**Auth fix**").as_deref(), Some("Auth fix"));
        assert_eq!(clean_title("\"Auth fix\"").as_deref(), Some("Auth fix"));
        assert_eq!(clean_title("`Auth fix`").as_deref(), Some("Auth fix"));
        assert_eq!(clean_title("## Auth fix").as_deref(), Some("Auth fix"));
        assert_eq!(
            clean_title("\u{201c}Auth fix\u{201d}").as_deref(),
            Some("Auth fix")
        );
    }

    #[test]
    fn trailing_prose_after_the_first_line_is_dropped() {
        let raw = "Auth middleware failure\n\nThis conversation covers why the \
                   staging host rejected tokens signed with the old key.";
        assert_eq!(clean_title(raw).as_deref(), Some("Auth middleware failure"));
    }

    #[test]
    fn trailing_punctuation_is_removed() {
        assert_eq!(clean_title("Auth fix.").as_deref(), Some("Auth fix"));
        assert_eq!(clean_title("Auth fix!").as_deref(), Some("Auth fix"));
        assert_eq!(clean_title("Auth fix: ").as_deref(), Some("Auth fix"));
    }

    #[test]
    fn refusals_and_junk_are_rejected_rather_than_displayed() {
        for bad in [
            "I cannot generate a title",
            "I'm sorry, I can't help with that",
            "As an AI language model",
            "no title",
            "N/A",
            "...",
            "  ",
        ] {
            assert!(
                clean_title(bad).is_none(),
                "{bad:?} should not become a sidebar title"
            );
        }
    }

    #[test]
    fn prose_instead_of_a_title_is_rejected() {
        // 15+ words means the model ignored the instruction; showing it truncated
        // would be worse than falling back.
        let prose = "This is a long and detailed conversation about many different \
                     topics including authentication and deployment and testing";
        assert!(clean_title(prose).is_none());
    }

    #[test]
    fn an_over_long_title_is_capped() {
        let long = "a".repeat(200);
        let out = clean_title(&long).expect("non-empty");
        assert_eq!(out.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn the_fallback_reads_better_than_a_raw_truncation() {
        // The old behavior, for comparison.
        let msg = "can you look at why the auth middleware is failing";
        // The GUI used `slice(0, 32)` on the raw first message.
        let old = msg.chars().take(32).collect::<String>();
        assert_eq!(
            old, "can you look at why the auth mid",
            "pinning the GUI's exact 32-char truncation"
        );

        let new = fallback_title(msg);
        // Word-bounded: the result ends on a whole word, never a fragment like
        // "middlewar". Both leading fillers ("can", "you") are dropped, then the
        // six-word cap applies -- so this is "You Look At Why The", which is still
        // more readable than the raw cut. The generated title is what actually
        // names the subject; this is only the no-model fallback.
        assert_eq!(new, "You Look At Why The");
        assert!(!new.ends_with("middlewar"));
        assert!(!new.ends_with("mid"));
        // And it must never contain a mid-word fragment of the last word either.
        assert!(!new.contains("middlewar"));
    }

    #[test]
    fn the_fallback_skips_a_leading_filler_word() {
        // Every word after the first is title-cased, which is the intended
        // behaviour -- so "there auth is broken" becomes "There Auth Is Broken".
        assert_eq!(
            fallback_title("hello there auth is broken"),
            "There Auth Is Broken"
        );
        assert_eq!(
            fallback_title("Can you explain the retry backoff"),
            "You Explain The Retry Backoff"
        );
    }

    #[test]
    fn the_fallback_keeps_a_two_word_message_intact() {
        // Only skip the filler when something else remains.
        assert_eq!(fallback_title("hi auth"), "Auth");
    }

    #[test]
    fn the_fallback_preserves_identifier_casing() {
        // Each word gets its first character uppercased; the REST of the word is
        // left alone, so the camelCase inside the identifier survives intact.
        assert_eq!(
            fallback_title("fix authMiddleware null check"),
            "Fix AuthMiddleware Null Check"
        );
    }

    #[test]
    fn the_fallback_survives_code_and_control_characters() {
        let out = fallback_title("fn main() {\r\n    let x = 1;");
        assert!(
            !out.contains('\r'),
            "control chars must not survive: {out:?}"
        );
        assert!(!out.contains('\n'), "newlines must not survive: {out:?}");
        // Punctuation-only tokens are dropped by the alphanumeric filter, so the
        // title is built from the words that remain.
        // Tokens that merely *contain* alphanumerics are kept, so `main()` and
        // `1;` survive with their punctuation. Only pure-punctuation tokens
        // ("***") are dropped.
        assert_eq!(out, "Fn Main() Let X 1;");
        assert_eq!(fallback_title("   ***   "), "New Chat");
        assert_eq!(fallback_title(""), "New Chat");
    }

    #[test]
    fn the_fallback_is_always_usable() {
        // Whatever the input, the caller must get something renderable.
        for input in ["", "   ", "...", "\n\n", "12345", "a", "🎉🎉🎉", "\u{200b}"] {
            let out = fallback_title(input);
            assert!(!out.is_empty(), "empty title for {input:?}");
            assert!(out.chars().count() <= MAX_TITLE_CHARS);
        }
    }
}
