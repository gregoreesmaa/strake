//! Fallback repair for bare contextual-keyword arrow params.
//!
//! Boa 0.22 rejects a bare `of` or `let` as a single arrow-function parameter
//! (`y(of => {...})`, `var f = let => 1`) although both are valid per spec and
//! routinely emitted by bundlers — esbuild's minifier picked `of` in Joplin's
//! `main.bundle.js`, which made the whole bundle unparseable. The
//! parenthesized form (`(of) =>`) parses fine and is semantics-preserving.
//!
//! [`repair_keyword_arrow_params`] drives the repair off the parser's own
//! diagnostics instead of re-lexing the source: while the source fails to
//! parse with an arrow-shaped error (`got '=>'`), it parenthesizes the
//! indicated parameter and re-parses, stopping at the first non-arrow error,
//! repeated position (no progress), or iteration cap. Only byte ranges the
//! parser itself pointed at are ever rewritten, so parsing code, strings,
//! comments, and regexes are never touched.
//!
//! It runs ONLY as a fallback after the original source already failed to
//! parse, so code that parses is never rewritten; callers must additionally
//! retry only on proven parse failures, so partially executed code never runs
//! twice.

/// Bare-identifier arrow params Boa 0.22 rejects: `of` and `let` parse fine
/// everywhere else (bindings, call args, parenthesized params) — only the
/// unparenthesized arrow-param position fails, proven by targeted probes.
const KEYWORDS: [&str; 2] = ["of", "let"];

/// Upper bound on repaired sites per source (Joplin's bundle needs one).
const MAX_REPAIRS: u32 = 1024;

/// Repair bare `of`/`let` arrow params using the parser's own error
/// positions. Returns the first source that parses (which may need zero
/// repairs only when `code` already parses — callers gate on failure first),
/// or `None` when the failure is not an arrow-shaped error or a repair site
/// fails verification.
pub(crate) fn repair_keyword_arrow_params(
    code: &str,
    context: &mut boa_engine::Context,
) -> Option<String> {
    let mut current = code.to_string();
    let mut last_pos: Option<(u32, u32)> = None;
    for _ in 0..MAX_REPAIRS {
        let Some(message) = parse_error_message(&current, context) else {
            return Some(current);
        };
        let (line, col) = arrow_error_position(&message)?;
        if last_pos == Some((line, col)) {
            return None;
        }
        last_pos = Some((line, col));
        current = splice_parens(&current, line, col).ok()?;
    }
    None
}

/// The parse error message for `code`, or `None` when it parses. The message
/// is extracted (and the error dropped) immediately so no borrow escapes.
fn parse_error_message(code: &str, context: &mut boa_engine::Context) -> Option<String> {
    use boa_engine::{Source, script::Script};
    match Script::parse(Source::from_bytes(code), None, context) {
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    }
}

/// Extract `(line, col)` from an arrow-shaped parse error
/// (`... got '=>' ... at line L, col C ...`), or `None` for anything else.
fn arrow_error_position(message: &str) -> Option<(u32, u32)> {
    if !message.contains("got '=>'") {
        return None;
    }
    let tail = message.rfind("at line ")?;
    let rest = message.get(tail + "at line ".len()..)?;
    let (line_text, rest) = rest.split_once(", col ")?;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    let (col_text, _) = rest.split_at(end);
    let line: u32 = line_text.parse().ok()?;
    let col: u32 = col_text.parse().ok()?;
    if line == 0 || col == 0 {
        return None;
    }
    Some((line, col))
}

/// Parenthesize the `of`/`let` parameter immediately before the `=>` at
/// (`line`, `col`) (both 1-based; the position points at the `=`).
/// Fails closed on any verification mismatch.
fn splice_parens(source: &str, line: u32, col: u32) -> Result<String, ()> {
    // Byte offset of the `=>` (columns count characters; converting via
    // `chars` keeps multibyte lines correct).
    let mut line_start = 0;
    for _ in 1..line {
        let rest = source.get(line_start..).ok_or(())?;
        let nl = rest.find('\n').ok_or(())?;
        line_start += nl + 1;
    }
    let line_text = source.get(line_start..).ok_or(())?;
    let line_text = line_text.split('\n').next().ok_or(())?;
    let mut arrow_at = line_start;
    let mut chars = line_text.chars();
    for _ in 0..col - 1 {
        let ch = chars.next().ok_or(())?;
        arrow_at += ch.len_utf8();
    }
    if source.get(arrow_at..arrow_at + 2) != Some("=>") {
        return Err(());
    }
    // Skip ASCII whitespace between the identifier and the `=>`, then scan
    // the parameter identifier backwards (ASCII fast path; `of`/`let` are
    // ASCII, and a preceding multibyte byte rejects below).
    let mut ident_end = arrow_at;
    while ident_end > 0
        && matches!(
            source.as_bytes().get(ident_end - 1),
            Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x0B)
        )
    {
        ident_end -= 1;
    }
    let mut ident_start = ident_end;
    while ident_start > 0 {
        let prev = source.as_bytes().get(ident_start - 1).ok_or(())?;
        if !(prev.is_ascii_alphanumeric() || *prev == b'_' || *prev == b'$') {
            break;
        }
        ident_start -= 1;
    }
    if ident_end <= ident_start {
        return Err(());
    }
    let ident = source.get(ident_start..ident_end).ok_or(())?;
    if !KEYWORDS.contains(&ident) {
        return Err(());
    }
    // The identifier must stand alone: reject member access (`.of`), private
    // names (`#of`), and multibyte identifier continuations, which the ASCII
    // scan above cannot see.
    if let Some(prev) = source.as_bytes().get(ident_start.wrapping_sub(1)) {
        if *prev == b'.' || *prev == b'#' || *prev >= 0x80 {
            return Err(());
        }
    }
    let mut repaired = String::with_capacity(source.len() + 2);
    repaired.push_str(source.get(..ident_start).ok_or(())?);
    repaired.push('(');
    repaired.push_str(ident);
    repaired.push(')');
    repaired.push_str(source.get(ident_end..).ok_or(())?);
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::{arrow_error_position, repair_keyword_arrow_params, splice_parens};

    fn repair(code: &str) -> Option<String> {
        let mut context = boa_engine::Context::default();
        repair_keyword_arrow_params(code, &mut context)
    }

    #[test]
    fn bare_of_and_let_arrows_repair_and_parse() {
        assert_eq!(
            repair("var zh = y(of => of + 1);"),
            Some("var zh = y((of) => of + 1);".to_string())
        );
        assert_eq!(
            repair("var f = let => 1;"),
            Some("var f = (let) => 1;".to_string())
        );
        // Multiple sites repair iteratively.
        assert_eq!(
            repair("var a = y(of => 1); var b = y(let => 2);"),
            Some("var a = y((of) => 1); var b = y((let) => 2);".to_string())
        );
    }

    #[test]
    fn non_arrow_failures_and_clean_code_pass_through() {
        // Already-valid code parses as-is (callers only invoke the repair on
        // failure, but the function itself is total).
        assert_eq!(
            repair("var f = x => x;"),
            Some("var f = x => x;".to_string())
        );
        // Non-arrow syntax errors are not ours to fix.
        assert_eq!(repair("var x = ;"), None);
        // Valid code (including `of` in non-param positions) needs no repair.
        assert_eq!(
            repair("for (const x of y) { z(x); }"),
            Some("for (const x of y) { z(x); }".to_string())
        );
        // A strict-mode `let` arrow stays an error: parenthesizing cannot
        // help, so the original failure stands.
        assert_eq!(repair("\"use strict\"; var f = let => 1;"), None);
    }

    #[test]
    fn error_positions_map_to_exact_sites() {
        assert_eq!(
            arrow_error_position(
                "expected one of ',' or ')', got '=>' in argument list at line 2, col 15 (native)"
            ),
            Some((2, 15))
        );
        assert_eq!(
            arrow_error_position("unexpected token ';' at line 1, col 9"),
            None
        );
        assert_eq!(
            splice_parens("var zh = y(of => of);", 1, 15),
            Ok("var zh = y((of) => of);".to_string())
        );
        // Wrong offsets fail closed instead of rewriting the wrong bytes.
        assert!(splice_parens("var zh = y(of => of);", 1, 16).is_err());
        assert!(splice_parens("var zh = y(of => of);", 2, 15).is_err());
        // Member access and longer identifiers are not params.
        assert!(splice_parens("a.of => 1;", 1, 6).is_err());
        assert!(splice_parens("var offer = x => 1;", 1, 15).is_err());
    }

    /// Canary on the engine gap this module exists for: if a future Boa
    /// accepts bare `of`-arrows, the eval fallback becomes dead code and
    /// should be removed.
    #[test]
    fn bare_of_arrow_fails_boa_parse() {
        let mut context = boa_engine::Context::default();
        let code = "function y(f) { return f; } var zh = y(of => of + 1);";
        let parsed = boa_engine::script::Script::parse(
            boa_engine::Source::from_bytes(code),
            None,
            &mut context,
        );
        assert!(parsed.is_err(), "bare of-arrow must fail Boa parse");
    }
}
