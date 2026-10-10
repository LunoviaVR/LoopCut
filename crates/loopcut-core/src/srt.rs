//! SubRip (`.srt`) parser.
//!
//! Accepted input:
//! * an optional UTF-8 byte order mark, CRLF or LF line endings, and any number of blank lines;
//! * optional cue numbers (a line of digits before the timing line);
//! * timing lines `HH:MM:SS,mmm --> HH:MM:SS,mmm` (a `.` decimal separator is accepted too), with any
//!   position or alignment settings after the end time, which are ignored;
//! * one or more text lines per cue, ended by a blank line, by the end of the file, or by the next timing line
//!   (a cue number on the line before it is not text either);
//! * HTML-like tags (`<i>`, `</b>`, `<font color="...">`) removed from the text, and the basic entities
//!   `&amp; &lt; &gt; &quot; &apos; &#39; &nbsp;` decoded. Anything else in angle brackets is kept. A tag body is
//!   read only up to 100 bytes past its `<`, so a long run of stray `<` costs time linear in the line length.
//!
//! Cues may overlap; the output keeps file order. A cue whose text is empty after tag removal is skipped.
//! Errors carry the 1-based line number of the problem.

use crate::time::TimeRange;

/// Largest input the parser accepts, in bytes. Real subtitle files are tens of kilobytes.
pub const MAX_SRT_BYTES: usize = 2 * 1024 * 1024;

/// Errors from [`parse_srt`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SrtError {
    /// The input is larger than [`MAX_SRT_BYTES`].
    #[error("subtitle file is {bytes} bytes; the limit is {max}")]
    TooLarge {
        /// Size of the input in bytes.
        bytes: usize,
        /// Largest accepted size in bytes.
        max: usize,
    },
    /// A line does not match the SubRip syntax, or a cue has an impossible time range.
    #[error("line {line}: {message}")]
    Syntax {
        /// 1-based line number.
        line: usize,
        /// What was wrong.
        message: String,
    },
}

/// Parses SubRip text into `(source range, text)` pairs in file order.
///
/// Times are seconds from the start of the file, as the file states them. Cues whose text is empty after
/// tag removal are left out. Text lines are joined with `\n`.
///
/// # Errors
///
/// [`SrtError::TooLarge`] for input over [`MAX_SRT_BYTES`]. [`SrtError::Syntax`] for a malformed cue, with the
/// line number of the first problem.
pub fn parse_srt(text: &str) -> Result<Vec<(TimeRange, String)>, SrtError> {
    if text.len() > MAX_SRT_BYTES {
        return Err(SrtError::TooLarge {
            bytes: text.len(),
            max: MAX_SRT_BYTES,
        });
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();

    let mut cues = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }

        // A cue starts with an optional number, then the timing line.
        let timing_idx = if lines[i].contains("-->") {
            i
        } else {
            let number = lines[i].trim();
            if !number.bytes().all(|b| b.is_ascii_digit()) {
                return Err(syntax(
                    i + 1,
                    "expected a cue number or a timing line with '-->'",
                ));
            }
            if i + 1 >= lines.len() {
                return Err(syntax(i + 1, "cue number without a timing line"));
            }
            i + 1
        };
        let (start, end) = parse_timing(lines[timing_idx], timing_idx + 1)?;

        // Text runs until a blank line, the end of the input, or the start of the next cue.
        let mut body = Vec::new();
        let mut j = timing_idx + 1;
        while j < lines.len() && !lines[j].trim().is_empty() {
            if starts_cue(&lines, j) {
                break;
            }
            body.push(clean_text(lines[j].trim()));
            j += 1;
        }
        i = j;

        let cue_text = body.join("\n");
        if !cue_text.trim().is_empty() {
            cues.push((TimeRange::new(start, end), cue_text));
        }
    }
    Ok(cues)
}

fn syntax(line: usize, message: &str) -> SrtError {
    SrtError::Syntax {
        line,
        message: message.to_owned(),
    }
}

/// True when `lines[j]` begins a cue: a valid timing line, or a cue number directly followed by one.
fn starts_cue(lines: &[&str], j: usize) -> bool {
    is_timing_line(lines[j])
        || (is_cue_number(lines[j]) && lines.get(j + 1).is_some_and(|next| is_timing_line(next)))
}

/// A timing line that parses. Lines that merely contain `-->` but do not parse stay ordinary text.
fn is_timing_line(text: &str) -> bool {
    text.contains("-->") && parse_timing(text, 0).is_ok()
}

/// A non-empty run of ASCII digits, the optional number that opens a cue.
fn is_cue_number(text: &str) -> bool {
    let number = text.trim();
    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
}

/// Parses `start --> end [settings]` into seconds. `line` is the 1-based line number, for errors.
fn parse_timing(text: &str, line: usize) -> Result<(f64, f64), SrtError> {
    let mut parts = text.split("-->");
    let (Some(start_text), Some(end_text), None) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(syntax(line, "timing line must contain exactly one '-->'"));
    };
    let start = parse_time(start_text.trim())
        .ok_or_else(|| syntax(line, &format!("invalid start time '{}'", start_text.trim())))?;
    let end_token = end_text.split_whitespace().next().unwrap_or("");
    let end = parse_time(end_token)
        .ok_or_else(|| syntax(line, &format!("invalid end time '{end_token}'")))?;
    if end <= start {
        return Err(syntax(line, "cue ends at or before it starts"));
    }
    Ok((start, end))
}

/// Parses `HH:MM:SS,mmm` (or with `.`) into seconds. Hours may have more than two digits; the milliseconds
/// may have one to three digits.
fn parse_time(text: &str) -> Option<f64> {
    let separator = text.rfind([',', '.'])?;
    let (clock, fraction) = (&text[..separator], &text[separator + 1..]);
    let mut parts = clock.split(':');
    let hours = digits(parts.next()?)?;
    let minutes = digits(parts.next()?)?;
    let seconds = digits(parts.next()?)?;
    if parts.next().is_some() || minutes >= 60 || seconds >= 60 {
        return None;
    }
    if fraction.is_empty() || fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let millis: u64 = fraction.parse().ok()?;
    let scale = 10u64.pow(fraction.len() as u32);
    Some((hours * 3600 + minutes * 60 + seconds) as f64 + millis as f64 / scale as f64)
}

/// Parses a non-empty run of ASCII digits (at most nine, so the value cannot overflow).
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Removes HTML-like tags and decodes the basic entities. Angle brackets that do not start a tag stay.
fn clean_text(line: &str) -> String {
    /// Longest tag body (between `<` and `>`) that is treated as markup.
    const MAX_TAG_BODY: usize = 100;

    let mut stripped = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        stripped.push_str(&rest[..open]);
        let after = &rest[open..];
        // Search only the bytes a tag body can occupy, so each '<' costs a bounded scan and a long run of stray
        // brackets stays linear. '<' and '>' are ASCII, so the offsets found here are character boundaries.
        let window = &after.as_bytes()[1..(MAX_TAG_BODY + 2).min(after.len())];
        match window.iter().position(|&b| b == b'>').map(|k| k + 1) {
            Some(close) if is_tag_body(&after[1..close]) => {
                rest = &after[close + 1..];
            }
            _ => {
                stripped.push('<');
                rest = &after[1..];
            }
        }
    }
    stripped.push_str(rest);

    stripped
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// A tag body starts with a letter, optionally after a closing slash (`i`, `/i`, `font color="red"`, `br/`).
fn is_tag_body(body: &str) -> bool {
    let name = body.strip_prefix('/').unwrap_or(body);
    name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cues(text: &str) -> Vec<(f64, f64, String)> {
        parse_srt(text)
            .expect("valid SubRip")
            .into_iter()
            .map(|(range, text)| (range.start, range.end, text))
            .collect()
    }

    #[test]
    fn parses_numbered_cues_with_crlf_and_bom() {
        let text = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nHello there\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nSecond\r\n";
        assert_eq!(
            cues(text),
            vec![
                (1.0, 2.5, "Hello there".to_owned()),
                (3.0, 4.0, "Second".to_owned()),
            ]
        );
    }

    #[test]
    fn multi_line_text_is_joined_with_newlines() {
        let text = "1\n00:00:00,000 --> 00:00:01,000\nTop line\nBottom line\n\n";
        assert_eq!(
            cues(text),
            vec![(0.0, 1.0, "Top line\nBottom line".to_owned())]
        );
    }

    #[test]
    fn cue_numbers_are_optional_and_the_last_cue_may_lack_a_blank_line() {
        let text =
            "00:00:00,500 --> 00:00:01,500\nno number\n\n00:00:02,000 --> 00:00:03,000\nlast";
        assert_eq!(
            cues(text),
            vec![
                (0.5, 1.5, "no number".to_owned()),
                (2.0, 3.0, "last".to_owned()),
            ]
        );
    }

    #[test]
    fn position_settings_after_the_end_time_are_ignored() {
        let text =
            "1\n00:00:01,000 --> 00:00:02,000 X1:10 X2:20 Y1:30 Y2:40 align:start\nPositioned\n";
        assert_eq!(cues(text), vec![(1.0, 2.0, "Positioned".to_owned())]);
    }

    #[test]
    fn html_like_tags_are_removed_and_other_brackets_kept() {
        let text = "1\n00:00:00,000 --> 00:00:01,000\n<i>Slanted</i> and <b>bold</b>\n<font color=\"#ff0\">yellow</font>\na < b > c and 3<4\n";
        assert_eq!(
            cues(text),
            vec![(
                0.0,
                1.0,
                "Slanted and bold\nyellow\na < b > c and 3<4".to_owned()
            )]
        );
    }

    #[test]
    fn basic_entities_are_decoded_after_tags() {
        let text = "1\n00:00:00,000 --> 00:00:01,000\nTom &amp; Jerry &lt;i&gt; &quot;hi&quot; it&#39;s&nbsp;fine\n";
        assert_eq!(
            cues(text),
            vec![(0.0, 1.0, "Tom & Jerry <i> \"hi\" it's fine".to_owned())]
        );
    }

    #[test]
    fn overlapping_cues_keep_file_order() {
        let text = "1\n00:00:00,000 --> 00:00:05,000\nA\n\n2\n00:00:02,000 --> 00:00:03,000\nB\n";
        assert_eq!(
            cues(text),
            vec![(0.0, 5.0, "A".to_owned()), (2.0, 3.0, "B".to_owned())]
        );
    }

    #[test]
    fn dot_separator_long_hours_and_short_milliseconds() {
        let text = "1\n10:00:00.5 --> 10:00:01,25\nLong\n";
        let parsed = cues(text);
        assert_eq!(parsed.len(), 1);
        assert!((parsed[0].0 - 36_000.5).abs() < 1e-9);
        assert!((parsed[0].1 - 36_001.25).abs() < 1e-9);
    }

    #[test]
    fn empty_cues_are_skipped_and_empty_input_is_fine() {
        let text =
            "1\n00:00:00,000 --> 00:00:01,000\n<i></i>\n\n2\n00:00:01,000 --> 00:00:02,000\nKept\n";
        assert_eq!(cues(text), vec![(1.0, 2.0, "Kept".to_owned())]);
        assert!(parse_srt("").expect("empty is valid").is_empty());
        assert!(parse_srt("\n\n  \n").expect("blank is valid").is_empty());
    }

    #[test]
    fn errors_carry_the_line_number() {
        let missing_arrow = "1\n00:00:00,000 00:00:01,000\nx\n";
        assert!(matches!(
            parse_srt(missing_arrow),
            Err(SrtError::Syntax { line: 2, .. })
        ));

        let garbage_line = "1\n00:00:00,000 --> 00:00:01,000\nok\n\nnot a cue\n";
        match parse_srt(garbage_line) {
            Err(SrtError::Syntax { line, message }) => {
                assert_eq!(line, 5);
                assert!(message.contains("cue number"), "message: {message}");
            }
            other => panic!("expected a syntax error, got {other:?}"),
        }

        let bad_minutes = "1\n00:61:00,000 --> 00:62:00,000\nx\n";
        assert!(matches!(
            parse_srt(bad_minutes),
            Err(SrtError::Syntax { line: 2, .. })
        ));

        let reversed = "1\n00:00:05,000 --> 00:00:01,000\nx\n";
        match parse_srt(reversed) {
            Err(SrtError::Syntax { line, message }) => {
                assert_eq!(line, 2);
                assert!(message.contains("before it starts"), "message: {message}");
            }
            other => panic!("expected a syntax error, got {other:?}"),
        }

        let number_at_end = "1\n00:00:00,000 --> 00:00:01,000\nx\n\n7";
        assert!(matches!(
            parse_srt(number_at_end),
            Err(SrtError::Syntax { line: 5, .. })
        ));
    }

    #[test]
    fn oversized_input_is_refused_before_parsing() {
        let big = "x".repeat(MAX_SRT_BYTES + 1);
        assert_eq!(
            parse_srt(&big),
            Err(SrtError::TooLarge {
                bytes: MAX_SRT_BYTES + 1,
                max: MAX_SRT_BYTES,
            })
        );
    }

    #[test]
    fn time_parsing_rejects_malformed_values() {
        let approx = |text: &str, expected: f64| {
            let value = parse_time(text).expect("valid time");
            assert!(
                (value - expected).abs() < 1e-9,
                "{text}: {value} != {expected}"
            );
        };
        approx("00:00:01,000", 1.0);
        approx("00:01:00,001", 60.001);
        approx("1:2:3,4", 3723.4);
        assert_eq!(parse_time("00:00:01"), None);
        assert_eq!(parse_time("00:00:01,"), None);
        assert_eq!(parse_time("00:00:01,1234"), None);
        assert_eq!(parse_time("00:00:60,000"), None);
        assert_eq!(parse_time("a:00:01,000"), None);
        assert_eq!(parse_time("1:2:3:4,000"), None);
    }

    #[test]
    fn a_cue_body_ends_at_the_next_timing_line_without_a_blank_line() {
        let numbered = "1\n00:00:00,000 --> 00:00:01,000\nfirst\n2\n00:00:02,000 --> 00:00:03,000\nsecond\n00:00:04,000 --> 00:00:05,000\nthird\n";
        assert_eq!(
            cues(numbered),
            vec![
                (0.0, 1.0, "first".to_owned()),
                (2.0, 3.0, "second".to_owned()),
                (4.0, 5.0, "third".to_owned()),
            ]
        );
        // Without cue numbers, a timing-like line that does not parse stays text, and digits that are not followed
        // by a timing line stay text too.
        let plain = "00:00:00,000 --> 00:00:01,000\nfirst line\nand 00:00:09,000 --> nowhere\n42\nstill text\n00:00:02,000 --> 00:00:03,000\nsecond\n";
        assert_eq!(
            cues(plain),
            vec![
                (
                    0.0,
                    1.0,
                    "first line\nand 00:00:09,000 --> nowhere\n42\nstill text".to_owned()
                ),
                (2.0, 3.0, "second".to_owned()),
            ]
        );
    }

    #[test]
    fn a_long_run_of_angle_brackets_is_scanned_in_linear_time() {
        let line = "<".repeat(1 << 20);
        let text = format!("1\n00:00:00,000 --> 00:00:01,000\n{line}\n");
        let started = std::time::Instant::now();
        let parsed = cues(&text);
        let elapsed = started.elapsed();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].2.len(), 1 << 20, "every bracket is kept as text");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "parsing a 1 MiB line of '<' took {elapsed:?}"
        );
        // A real tag after a long run is still removed.
        let mixed = format!(
            "1\n00:00:00,000 --> 00:00:01,000\n{}<i>x</i>\n",
            "<".repeat(300)
        );
        let expected = format!("{}x", "<".repeat(300));
        assert_eq!(cues(&mixed)[0].2, expected);
    }
}
