//! Undo Whisper's repetition loops in the text it returns.
//!
//! A looping decoder emits one phrase over and over until the token cap stops
//! it (`asr.rs` keeps that cap close to what the audio could hold). What comes
//! back is the real start of the utterance followed by dozens of copies of one
//! phrase, usually cut off partway through the last copy. Keeping a single
//! copy keeps the part that was probably said and drops the part that
//! certainly was not.
//!
//! The thresholds let ordinary repetition through — 「はいはいはい」, a phrase
//! said twice, a short laugh. Only a run of at least [`MIN_COPIES`] copies
//! spanning at least [`MIN_RUN_CHARS`] characters is collapsed.

const MIN_COPIES: usize = 4;
const MIN_RUN_CHARS: usize = 20;
/// The longest phrase looked for. The loop seen in a real meeting was 16
/// characters; this leaves room for a whole sentence.
const MAX_UNIT_CHARS: usize = 64;

/// Collapse every run of one phrase repeated back to back into a single copy.
///
/// Returns the cleaned text and how many characters were removed.
pub fn collapse_repeats(text: &str) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    let mut i = 0;
    while i < chars.len() {
        match longest_run(&chars[i..]) {
            Some((unit, covered)) => {
                out.extend(&chars[i..i + unit]);
                removed += covered - unit;
                i += covered;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    (out, removed)
}

/// The longest qualifying run at the start of `rest`, as
/// `(phrase length, characters covered)`. On a tie the shorter phrase wins, so
/// 「ああああ…」 collapses to 「あ」 rather than 「ああ」.
fn longest_run(rest: &[char]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for unit in 1..=MAX_UNIT_CHARS.min(rest.len() / MIN_COPIES) {
        let phrase = &rest[..unit];
        let mut copies = 1;
        while rest.len() >= (copies + 1) * unit && rest[copies * unit..(copies + 1) * unit] == *phrase
        {
            copies += 1;
        }
        let mut covered = copies * unit;
        if copies < MIN_COPIES || covered < MIN_RUN_CHARS {
            continue;
        }
        // The token cap usually stops a loop partway through a copy. Drop that
        // fragment too, but only at the very end of the text: in the middle,
        // what follows a run is real speech that may happen to start the same.
        let tail = &rest[covered..];
        if tail.len() < unit && phrase.starts_with(tail) {
            covered += tail.len();
        }
        if best.map_or(true, |(_, c)| covered > c) {
            best = Some((unit, covered));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::collapse_repeats;

    const PHRASE: &str = "それを前提にご説明いただいた上で、";

    #[test]
    fn a_loop_cut_off_mid_copy_keeps_one_copy() {
        let mut text = String::from("では次に、");
        for _ in 0..30 {
            text.push_str(PHRASE);
        }
        text.push_str("それを前提にご説明");
        let (out, removed) = collapse_repeats(&text);
        // Where the copy starts is not pinned down: the prefix ends in the
        // phrase's last character, so the run is equally a run of the phrase
        // rotated by one (「、それを…上で」), and that one starts first.
        assert!(out.starts_with("では次に"), "{out}");
        assert_eq!(out.matches("それを前提にご説明いただいた上で").count(), 1, "{out}");
        assert!(out.chars().count() <= "では次に、".chars().count() + PHRASE.chars().count());
        assert_eq!(removed, text.chars().count() - out.chars().count());
    }

    #[test]
    fn speech_after_a_run_is_kept() {
        let text = format!("{}以上です。", PHRASE.repeat(5));
        let (out, _) = collapse_repeats(&text);
        assert_eq!(out, format!("{PHRASE}以上です。"));
    }

    #[test]
    fn a_single_character_loop_collapses_to_one() {
        let (out, removed) = collapse_repeats(&"あ".repeat(40));
        assert_eq!(out, "あ");
        assert_eq!(removed, 39);
    }

    #[test]
    fn ordinary_repetition_is_left_alone() {
        for text in [
            "",
            "本日の定例ミーティングを始めます",
            "はいはいはいはい",
            "ははははははは",
            "ありがとうございます、ありがとうございます",
            "そうですね、そうですね、そうですね",
            "いやいやいや、そういうことではなくて",
        ] {
            assert_eq!(collapse_repeats(text), (text.to_string(), 0), "{text}");
        }
    }
}
