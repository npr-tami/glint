//! Colours for code and settings files in the Space preview (preview.rs): comments, strings,
//! numbers and keywords, chosen by the file's extension. Hand-written and small, so it knows each
//! language only roughly - enough to make a file easier to read, not to check it.
//!
//! It works one token at a time ([`step`]), so the preview colours only the rows on screen: a
//! [`Painter`] remembers where each row's colouring starts (inside a comment or a string that began
//! rows earlier, say), worked out once as the rows are first drawn.

use crate::theme::{Color, Theme};

/// What a piece of text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Plain,
    Comment,
    String,
    Number,
    Keyword,
}

/// What goes on from one token to the next, and from one line to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct State {
    inside: Inside,
    /// Markup only: between a tag's "<" and its ">".
    tag: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Inside {
    #[default]
    Code,
    /// In the language's block comment number n.
    Block(u8),
    /// In a string opened by the language's quote number n.
    Str(u8),
}

/// How one language writes its comments, strings and keywords.
pub struct Lang {
    /// What starts a comment that runs to the end of the line.
    line: &'static [&'static str],
    /// What opens and closes a block comment.
    block: &'static [(&'static str, &'static str)],
    /// What opens (and closes) a string, longest first ("""" before ").
    quotes: &'static [&'static str],
    /// The quotes whose strings can run over several lines; the others end with their line.
    multiline: &'static [&'static str],
    /// What makes the next character part of a string (\" stays inside it).
    escape: Option<u8>,
    /// ' is a character ('a', '\n'), so on its own (a Rust lifetime) it's no string.
    chars: bool,
    /// A ' right after a letter is an apostrophe ("Don't" in JSX's text, a YAML value), no string.
    apostrophes: bool,
    keywords: &'static [&'static str],
    /// Keywords in any capitals (batch files, PowerShell).
    any_case: bool,
    /// A line comment starts only at the start of a word ("#" in a shell script; not "$#").
    comment_word_start: bool,
    /// A line comment starts only at the start of a line (ini's ";", a batch file's REM and ::).
    comment_line_start: bool,
    /// HTML and XML: tag names and the strings inside tags; the text between tags stays plain.
    markup: bool,
    numbers: bool,
    /// Markdown: a line starting with # is a heading.
    headings: bool,
}

const C_LIKE_BLOCK: &[(&str, &str)] = &[("/*", "*/")];
const MARKUP_BLOCK: &[(&str, &str)] = &[("<!--", "-->")];
const BASE: Lang = Lang {
    line: &[],
    block: &[],
    quotes: &["\"", "'"],
    multiline: &[],
    escape: Some(b'\\'),
    chars: false,
    apostrophes: false,
    keywords: &[],
    any_case: false,
    comment_word_start: false,
    comment_line_start: false,
    markup: false,
    numbers: true,
    headings: false,
};

static RUST: Lang = Lang {
    line: &["//"],
    block: C_LIKE_BLOCK,
    multiline: &["\""],
    chars: true,
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
    ],
    ..BASE
};
static PYTHON: Lang = Lang {
    line: &["#"],
    quotes: &["\"\"\"", "'''", "\"", "'"],
    multiline: &["\"\"\"", "'''"],
    keywords: &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from",
        "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try", "while", "with", "yield",
    ],
    ..BASE
};
const JS_WORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "export", "extends", "false", "finally", "for",
    "from", "function", "if", "import", "in", "instanceof", "let", "new", "null", "of", "return", "static", "super", "switch", "this", "throw", "true", "try", "typeof",
    "undefined", "var", "void", "while", "with", "yield",
];
static JS: Lang = Lang { line: &["//"], block: C_LIKE_BLOCK, quotes: &["\"", "'", "`"], multiline: &["`"], apostrophes: true, keywords: JS_WORDS, ..BASE };
static TS: Lang = Lang {
    keywords: &[
        "abstract", "any", "as", "async", "await", "boolean", "break", "case", "catch", "class", "const", "continue", "declare", "default", "delete", "do", "else", "enum",
        "export", "extends", "false", "finally", "for", "from", "function", "if", "implements", "import", "in", "instanceof", "interface", "keyof", "let", "namespace",
        "never", "new", "null", "number", "of", "private", "protected", "public", "readonly", "return", "static", "string", "super", "switch", "this", "throw", "true",
        "try", "type", "typeof", "undefined", "unknown", "var", "void", "while", "yield",
    ],
    ..JS
};
static JSON: Lang = Lang { line: &["//"], block: C_LIKE_BLOCK, quotes: &["\""], keywords: &["true", "false", "null"], ..BASE };
static MARKUP: Lang = Lang { block: MARKUP_BLOCK, escape: None, markup: true, numbers: false, ..BASE };
static CSS: Lang = Lang { block: C_LIKE_BLOCK, keywords: &["important", "media", "import", "keyframes", "supports", "charset", "inherit", "initial", "none", "auto"], ..BASE };
static C: Lang = Lang {
    line: &["//"],
    block: C_LIKE_BLOCK,
    chars: true,
    keywords: &[
        "auto", "bool", "break", "case", "char", "const", "continue", "default", "define", "do", "double", "elif", "else", "endif", "enum", "extern", "false", "float", "for",
        "goto", "if", "ifdef", "ifndef", "include", "inline", "int", "long", "NULL", "pragma", "register", "return", "short", "signed", "sizeof", "static", "struct",
        "switch", "true", "typedef", "union", "unsigned", "void", "volatile", "while",
    ],
    ..BASE
};
static CPP: Lang = Lang {
    keywords: &[
        "auto", "bool", "break", "case", "catch", "char", "class", "const", "constexpr", "continue", "default", "define", "delete", "do", "double", "else", "endif",
        "enum", "explicit", "extern", "false", "float", "for", "friend", "goto", "if", "ifdef", "ifndef", "include", "inline", "int", "long", "namespace", "new",
        "noexcept", "nullptr", "operator", "override", "pragma", "private", "protected", "public", "return", "short", "signed", "sizeof", "static", "struct", "switch",
        "template", "this", "throw", "true", "try", "typedef", "typename", "union", "unsigned", "using", "virtual", "void", "volatile", "while",
    ],
    ..C
};
static CSHARP: Lang = Lang {
    keywords: &[
        "abstract", "as", "async", "await", "base", "bool", "break", "byte", "case", "catch", "char", "class", "const", "continue", "decimal", "default", "delegate", "do",
        "double", "else", "enum", "event", "false", "finally", "float", "for", "foreach", "get", "if", "in", "int", "interface", "internal", "is", "lock", "long",
        "namespace", "new", "null", "object", "out", "override", "private", "protected", "public", "readonly", "ref", "return", "sealed", "set", "static", "string",
        "struct", "switch", "this", "throw", "true", "try", "typeof", "using", "var", "virtual", "void", "while",
    ],
    ..C
};
static JAVA: Lang = Lang {
    keywords: &[
        "abstract", "boolean", "break", "byte", "case", "catch", "char", "class", "continue", "default", "do", "double", "else", "enum", "extends", "false", "final",
        "finally", "float", "for", "if", "implements", "import", "instanceof", "int", "interface", "long", "new", "null", "package", "private", "protected", "public",
        "record", "return", "short", "static", "super", "switch", "synchronized", "this", "throw", "throws", "true", "try", "var", "void", "volatile", "while",
    ],
    ..C
};
static GO: Lang = Lang {
    quotes: &["\"", "'", "`"],
    multiline: &["`"],
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "false", "for", "func", "go", "goto", "if", "import", "interface", "map",
        "nil", "package", "range", "return", "select", "struct", "switch", "true", "type", "var",
    ],
    ..C
};
static SHELL: Lang = Lang {
    line: &["#"],
    multiline: &["\"", "'"],
    comment_word_start: true,
    keywords: &[
        "break", "case", "continue", "declare", "do", "done", "echo", "elif", "else", "esac", "exit", "export", "fi", "for", "function", "if", "in", "local", "readonly",
        "return", "select", "shift", "source", "then", "until", "unset", "while",
    ],
    ..BASE
};
static BATCH: Lang = Lang {
    line: &["::", "rem"],
    quotes: &["\""],
    escape: None,
    any_case: true,
    comment_line_start: true,
    keywords: &[
        "call", "cd", "defined", "do", "echo", "else", "endlocal", "equ", "errorlevel", "exist", "exit", "for", "geq", "goto", "gtr", "if", "in", "leq", "lss", "neq",
        "not", "off", "on", "pause", "set", "setlocal", "shift",
    ],
    ..BASE
};
static POWERSHELL: Lang = Lang {
    line: &["#"],
    block: &[("<#", "#>")],
    multiline: &["\"", "'"],
    escape: Some(b'`'),
    any_case: true,
    keywords: &[
        "begin", "break", "catch", "class", "continue", "do", "dynamicparam", "else", "elseif", "end", "enum", "exit", "false", "filter", "finally", "for", "foreach",
        "function", "if", "in", "null", "param", "process", "return", "switch", "throw", "trap", "true", "try", "until", "using", "while",
    ],
    ..BASE
};
static TOML: Lang = Lang { line: &["#"], quotes: &["\"\"\"", "'''", "\"", "'"], multiline: &["\"\"\"", "'''"], keywords: &["true", "false"], ..BASE };
static YAML: Lang = Lang { line: &["#"], comment_word_start: true, apostrophes: true, keywords: &["true", "false", "null", "yes", "no"], ..BASE };
static INI: Lang = Lang { line: &[";", "#"], quotes: &["\""], escape: None, comment_line_start: true, ..BASE };
static MARKDOWN: Lang = Lang { block: MARKUP_BLOCK, quotes: &["`"], escape: None, numbers: false, headings: true, ..BASE };

/// The language of a file with this extension (no dot, any capitals), or None to leave it plain.
pub fn lang_for(ext: &str) -> Option<&'static Lang> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => &RUST,
        "py" | "pyw" => &PYTHON,
        "js" | "jsx" | "mjs" | "cjs" => &JS,
        "ts" | "tsx" => &TS,
        "json" => &JSON,
        "html" | "htm" | "xml" => &MARKUP,
        "css" => &CSS,
        "c" | "h" => &C,
        "cpp" | "hpp" | "cc" | "cxx" | "hh" => &CPP,
        "cs" => &CSHARP,
        "java" => &JAVA,
        "go" => &GO,
        "sh" | "bash" => &SHELL,
        "bat" | "cmd" => &BATCH,
        "ps1" | "psm1" => &POWERSHELL,
        "toml" => &TOML,
        "yaml" | "yml" => &YAML,
        "ini" => &INI,
        "md" | "markdown" => &MARKDOWN,
        _ => return None,
    })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The token at byte `pos` of `line` (which it can't be past), in `state`: where it ends, what it
/// is, and the state after it. Every call moves on at least one character.
pub fn step(lang: &Lang, line: &str, pos: usize, state: State) -> (usize, Kind, State) {
    let rest = &line[pos..];
    let next_char = |at: usize| at + line[at..].chars().next().map_or(1, char::len_utf8);
    match state.inside {
        Inside::Block(n) => {
            let close = lang.block.get(n as usize).map_or("", |b| b.1);
            match rest.find(close).filter(|_| !close.is_empty()) {
                Some(i) => (pos + i + close.len(), Kind::Comment, State { inside: Inside::Code, ..state }),
                None => (line.len(), Kind::Comment, state),
            }
        }
        Inside::Str(n) => {
            let quote = lang.quotes.get(n as usize).copied().unwrap_or("\"");
            let bytes = rest.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if lang.escape == Some(bytes[i]) {
                    i += 2;
                    continue;
                }
                if bytes[i..].starts_with(quote.as_bytes()) {
                    return (pos + i + quote.len(), Kind::String, State { inside: Inside::Code, ..state });
                }
                i += 1;
            }
            // Unfinished: on to the next line only for a quote that can do that.
            let inside = if lang.multiline.contains(&quote) { state.inside } else { Inside::Code };
            (line.len(), Kind::String, State { inside, ..state })
        }
        Inside::Code => code_step(lang, line, pos, state, next_char),
    }
}

fn code_step(lang: &Lang, line: &str, pos: usize, state: State, next_char: impl Fn(usize) -> usize) -> (usize, Kind, State) {
    let rest = &line[pos..];
    let Some(c) = rest.chars().next() else { return (line.len().max(pos + 1), Kind::Plain, state) };
    let before = line[..pos].chars().next_back();
    // (Comments that must start a line are looked for only near its start: a long line isn't
    // gone through again for each ";" in it.)
    let line_start = || pos <= 256 && line[..pos].chars().all(|c| c.is_whitespace() || c == '@');
    if lang.headings && pos == 0 && c == '#' {
        return (line.len(), Kind::Keyword, state);
    }
    if c.is_whitespace() {
        let end = rest.find(|c: char| !c.is_whitespace()).map_or(line.len(), |i| pos + i);
        return (end, Kind::Plain, state);
    }
    for (n, (open, _)) in lang.block.iter().enumerate() {
        if rest.starts_with(open) {
            return (pos + open.len(), Kind::Comment, State { inside: Inside::Block(n as u8), ..state });
        }
    }
    if lang.markup && !state.tag {
        // Between tags: plain text up to the next tag (or comment).
        if c == '<' && rest[1..].starts_with(|c: char| c.is_alphabetic() || matches!(c, '/' | '!' | '?')) {
            let name = rest[1..].find(|c: char| !(c.is_alphanumeric() || matches!(c, '/' | '!' | '?' | '-' | ':' | '_' | '.'))).map_or(line.len(), |i| pos + 1 + i);
            return (name, Kind::Keyword, State { tag: true, ..state });
        }
        let first = c.len_utf8();
        let end = rest[first..].find('<').map_or(line.len(), |i| pos + first + i);
        return (end, Kind::Plain, state);
    }
    if lang.markup && (rest.starts_with("/>") || rest.starts_with("?>") || c == '>') {
        let end = if c == '>' { pos + 1 } else { pos + 2 };
        return (end, Kind::Keyword, State { tag: false, ..state });
    }
    for open in lang.line {
        let word_like = open.starts_with(|c: char| c.is_alphabetic());
        let matches = if lang.any_case { rest.get(..open.len()).is_some_and(|s| s.eq_ignore_ascii_case(open)) } else { rest.starts_with(open) };
        if !matches || (word_like && rest[open.len()..].starts_with(is_word)) {
            continue;
        }
        if (lang.comment_line_start && !line_start()) || (lang.comment_word_start && before.is_some_and(|b| !b.is_whitespace())) {
            continue;
        }
        return (line.len(), Kind::Comment, state);
    }
    for (n, quote) in lang.quotes.iter().enumerate() {
        if !rest.starts_with(quote) {
            continue;
        }
        if lang.apostrophes && *quote == "'" && before.is_some_and(char::is_alphanumeric) {
            return (pos + 1, Kind::Plain, state);
        }
        if lang.chars && *quote == "'" {
            // 'a' or '\n' (or '\u{1F600}'): a character. Anything else (a lifetime): just the '.
            let body = &rest[1..];
            let end = if body.starts_with('\\') {
                body.find('\'').filter(|&i| i > 1 && i <= 12).map(|i| pos + 1 + i + 1)
            } else {
                body.chars().next().filter(|&c| c != '\'').and_then(|c| body[c.len_utf8()..].starts_with('\'').then(|| pos + 1 + c.len_utf8() + 1))
            };
            return match end {
                Some(end) => (end, Kind::String, state),
                None => (pos + 1, Kind::Plain, state),
            };
        }
        return (pos + quote.len(), Kind::String, State { inside: Inside::Str(n as u8), ..state });
    }
    if is_word(c) {
        let end = rest.find(|c: char| !is_word(c)).map_or(line.len(), |i| pos + i);
        let word = &line[pos..end];
        if lang.numbers && c.is_ascii_digit() && !before.is_some_and(is_word) {
            // 12, 0x1F, 3.5e10, 10px: on through a decimal point.
            let mut end = end;
            while line[end..].starts_with('.') && line[end + 1..].starts_with(|c: char| c.is_ascii_digit()) {
                end = line[end + 1..].find(|c: char| !is_word(c)).map_or(line.len(), |i| end + 1 + i);
            }
            return (end, Kind::Number, state);
        }
        let keyword = if lang.any_case { lang.keywords.iter().any(|k| k.eq_ignore_ascii_case(word)) } else { lang.keywords.contains(&word) };
        return (end, if keyword && !lang.markup { Kind::Keyword } else { Kind::Plain }, state);
    }
    (next_char(pos), Kind::Plain, state)
}

/// The state a line ends in, carried to the next line: strings that can't run over lines end here.
fn at_line_end(lang: &Lang, state: State) -> State {
    match state.inside {
        Inside::Str(n) if !lang.quotes.get(n as usize).is_some_and(|q| lang.multiline.contains(q)) => State { inside: Inside::Code, ..state },
        _ => state,
    }
}

/// Where colouring a row picks up: at byte `at` of its line, in `state`, with what's before `at`
/// in the row (the end of a token that began on the row before) coloured as `carry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mark {
    at: u32,
    state: State,
    carry: Kind,
}

/// Colours the rows of one text as they're drawn. Rows are (line, its first byte, its end byte),
/// top to bottom, every line with at least one row; where each starts is worked out once, the
/// first time it or a row below it is drawn.
pub struct Painter {
    lang: &'static Lang,
    marks: Vec<Mark>,
}

impl Painter {
    pub fn new(lang: &'static Lang) -> Painter {
        Painter { lang, marks: vec![Mark { at: 0, state: State::default(), carry: Kind::Plain }] }
    }

    /// The coloured runs of bytes `from` to `to` of row `n`, as (start, end, kind) in bytes of its
    /// line, in order and without gaps (neighbouring runs of one kind come as one). `row(k)` gives
    /// row k; `lines` is the text.
    pub fn runs(&mut self, n: usize, row: impl Fn(usize) -> Option<(usize, usize, usize)>, lines: &[String], from: usize, to: usize, out: &mut Vec<(usize, usize, Kind)>) {
        out.clear();
        while self.marks.len() <= n {
            let k = self.marks.len() - 1;
            let (Some((line, _, end)), Some((next_line, _, _))) = (row(k), row(k + 1)) else { return };
            let text = lines[line].as_str();
            let Mark { at, mut state, mut carry } = self.marks[k];
            let mut pos = at as usize;
            // On to this row's end (the next row's start), or the line's end before a new line.
            // A token that runs over it colours the next row's start too.
            let target = if next_line == line { end } else { text.len() };
            while pos < target {
                let (e, kind, s) = step(self.lang, text, pos, state);
                (pos, carry, state) = (e.min(text.len()).max(pos + 1), kind, s);
            }
            self.marks.push(if next_line == line { Mark { at: pos as u32, state, carry } } else { Mark { at: 0, state: at_line_end(self.lang, state), carry: Kind::Plain } });
        }
        let Some((line, start, _)) = row(n) else { return };
        let text = lines[line].as_str();
        let to = to.min(text.len());
        let mut push = |a: usize, b: usize, kind: Kind| {
            let (a, b) = (a.max(from), b.min(to));
            if a < b {
                match out.last_mut() {
                    Some(last) if last.2 == kind && last.1 == a => last.1 = b,
                    _ => out.push((a, b, kind)),
                }
            }
        };
        let m = self.marks[n];
        let mut pos = m.at as usize;
        push(start, pos, m.carry);
        let mut state = m.state;
        while pos < to {
            let (end, kind, s) = step(self.lang, text, pos, state);
            let end = end.min(text.len()).max(pos + 1);
            push(pos, end, kind);
            (pos, state) = (end, s);
        }
    }
}

/// The colour for a kind of text, from the skin as the sort editor's rules box takes its colours:
/// comments muted, keywords in the accent, strings and numbers in the strong text colour (or, in a
/// skin where that's the same as the plain one, the plain one a little toward the accent).
/// `plain` is the colour of the rest.
pub fn color(theme: &Theme, kind: Kind, plain: Color) -> Color {
    match kind {
        Kind::Plain => plain,
        Kind::Comment => theme.muted,
        Kind::Keyword => theme.accent,
        Kind::String | Kind::Number => {
            let (a, b) = (theme.text_strong, theme.text);
            let apart = (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs();
            if apart >= 0.12 {
                a
            } else {
                b.mix(theme.accent.with_alpha(b.a), 0.45)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows of `cols` bytes each (a line with none is one empty row), as the preview's wrapped rows.
    fn rows_of(lines: &[String], cols: usize) -> Vec<(usize, usize, usize)> {
        let mut rows = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let mut start = 0;
            loop {
                let mut end = (start + cols).min(line.len());
                while !line.is_char_boundary(end) {
                    end += 1;
                }
                rows.push((i, start, end));
                if end >= line.len() {
                    break;
                }
                start = end;
            }
        }
        rows
    }

    /// Every coloured piece of the text, as (line, text, kind), each row coloured on its own.
    fn colour(ext: &str, text: &str, cols: usize) -> Vec<(usize, String, Kind)> {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let rows = rows_of(&lines, cols);
        let mut painter = Painter::new(lang_for(ext).expect("known"));
        let mut out: Vec<(usize, String, Kind)> = Vec::new();
        let mut runs = Vec::new();
        for (n, &(line, start, end)) in rows.iter().enumerate() {
            painter.runs(n, |k| rows.get(k).copied(), &lines, start, end, &mut runs);
            // Without gaps, from the row's start to its end.
            assert_eq!(runs.first().map_or(start, |r| r.0), start, "{ext} row {n}");
            assert_eq!(runs.last().map_or(start, |r| r.1), end, "{ext} row {n}");
            assert!(runs.windows(2).all(|w| w[0].1 == w[1].0), "{ext} row {n}: {runs:?}");
            for &(a, b, kind) in &runs {
                // A token cut by the row's end goes on at the next row's start.
                match out.last_mut() {
                    Some(last) if last.0 == line && last.2 == kind && start == a && a > 0 => last.1.push_str(&lines[line][a..b]),
                    _ => out.push((line, lines[line][a..b].to_string(), kind)),
                }
            }
        }
        out
    }

    /// The coloured (not plain) pieces.
    fn marked(ext: &str, text: &str) -> Vec<(String, Kind)> {
        colour(ext, text, 10_000).into_iter().filter(|p| p.2 != Kind::Plain).map(|p| (p.1, p.2)).collect()
    }

    fn of(pieces: &[(&str, Kind)]) -> Vec<(String, Kind)> {
        pieces.iter().map(|&(t, k)| (t.to_string(), k)).collect()
    }

    use Kind::{Comment as C, Keyword as K, Number as N, String as S};

    #[test]
    fn each_language_has_its_comments_strings_numbers_and_keywords() {
        let cases: &[(&str, &str, &[(&str, Kind)])] = &[
            ("rs", "let x = \"a\"; // note", &[("let", K), ("\"a\"", S), ("// note", C)]),
            ("rs", "/* one */ fn f() -> u8 { 42 }", &[("/* one */", C), ("fn", K), ("42", N)]),
            ("py", "def f(): return 'x'  # done", &[("def", K), ("return", K), ("'x'", S), ("# done", C)]),
            ("py", "s = \"\"\"doc\"\"\"", &[("\"\"\"doc\"\"\"", S)]),
            ("js", "const t = `a ${b}`; // c", &[("const", K), ("`a ${b}`", S), ("// c", C)]),
            ("ts", "interface A { x: number } /* y */", &[("interface", K), ("number", K), ("/* y */", C)]),
            ("jsx", "return <a href='x'>Don't</a>;", &[("return", K), ("'x'", S)]),
            ("tsx", "let n = 0x1F;", &[("let", K), ("0x1F", N)]),
            ("json", "{\"a\": 1.5, \"b\": true, \"c\": null}", &[("\"a\"", S), ("1.5", N), ("\"b\"", S), ("true", K), ("\"c\"", S), ("null", K)]),
            ("html", "<p class=\"x\">It's 5</p><!-- c -->", &[("<p", K), ("\"x\"", S), (">", K), ("</p>", K), ("<!-- c -->", C)]),
            ("htm", "<br/>", &[("<br/>", K)]),
            ("xml", "<?xml version='1.0'?><a/>", &[("<?xml", K), ("'1.0'", S), ("?><a/>", K)]),
            ("yaml", "msg: it's 'quoted'", &[("'quoted'", S)]),
            ("css", "a { width: 10px !important; } /* c */", &[("10px", N), ("important", K), ("/* c */", C)]),
            ("c", "#include \"x.h\" // y", &[("include", K), ("\"x.h\"", S), ("// y", C)]),
            ("h", "int c = 'a';", &[("int", K), ("'a'", S)]),
            ("cpp", "auto p = nullptr; /* z */", &[("auto", K), ("nullptr", K), ("/* z */", C)]),
            ("hpp", "class A {};", &[("class", K)]),
            ("cs", "var s = \"q\"; // c", &[("var", K), ("\"q\"", S), ("// c", C)]),
            ("java", "public int x = 3;", &[("public", K), ("int", K), ("3", N)]),
            ("go", "func f() { s := `raw` } // c", &[("func", K), ("`raw`", S), ("// c", C)]),
            ("sh", "echo \"$#\" $# # note", &[("echo", K), ("\"$#\"", S), ("# note", C)]),
            ("bat", "@REM note", &[("REM note", C)]),
            ("bat", "IF EXIST \"a b\" echo remove :: x", &[("IF", K), ("EXIST", K), ("\"a b\"", S), ("echo", K)]),
            ("cmd", ":: note", &[(":: note", C)]),
            ("ps1", "if ($true) { \"a`\"b\" } # c", &[("if", K), ("true", K), ("\"a`\"b\"", S), ("# c", C)]),
            ("ps1", "<# block #> Write-Host", &[("<# block #>", C)]),
            ("toml", "a = 'x' # c\nb = true", &[("'x'", S), ("# c", C), ("true", K)]),
            ("yaml", "a: \"x\" # c\nb: url#frag", &[("\"x\"", S), ("# c", C)]),
            ("yml", "on: yes", &[("yes", K)]),
            ("ini", "; c\na=1 ; not a comment\n  # c2", &[("; c", C), ("1", N), ("# c2", C)]),
            ("md", "# Title\nUse `code` <!-- c --> 42", &[("# Title", K), ("`code`", S), ("<!-- c -->", C)]),
        ];
        for (ext, text, want) in cases {
            assert_eq!(marked(ext, text), of(want), "{ext}: {text}");
        }
    }

    #[test]
    fn unknown_types_stay_plain() {
        for ext in ["txt", "log", "", "rs.bak", "unknown"] {
            assert!(lang_for(ext).is_none(), "{ext}");
        }
        assert!(lang_for("RS").is_some() && lang_for("Json").is_some());
    }

    #[test]
    fn unfinished_strings_and_comments_end_where_their_language_says() {
        // A C string ends with its line; a block comment goes on to the end of the file.
        assert_eq!(marked("c", "char *s = \"open\nint x;"), of(&[("char", K), ("\"open", S), ("int", K)]));
        assert_eq!(marked("c", "/* open\nint x;\nstill"), of(&[("/* open", C), ("int x;", C), ("still", C)]));
        // Python's triple quotes and JavaScript's backticks go over lines; a plain quote doesn't.
        assert_eq!(marked("py", "x = \"\"\"one\ntwo\"\"\" + 'a\nif"), of(&[("\"\"\"one", S), ("two\"\"\"", S), ("'a", S), ("if", K)]));
        assert_eq!(marked("js", "`a\nb` + \"c\nlet"), of(&[("`a", S), ("b`", S), ("\"c", S), ("let", K)]));
        // A quote at the very end of a line, and an escape there.
        assert_eq!(marked("rs", "let s = \"\\"), of(&[("let", K), ("\"\\", S)]));
        assert_eq!(marked("json", "\"a\\\""), of(&[("\"a\\\"", S)]));
    }

    #[test]
    fn quotes_inside_strings_and_lifetimes() {
        assert_eq!(marked("py", "\"say 'hi'\" + 'it\\'s'"), of(&[("\"say 'hi'\"", S), ("'it\\'s'", S)]));
        assert_eq!(marked("js", "'a \"b\" c' \"d \\\"e\\\" f\""), of(&[("'a \"b\" c'", S), ("\"d \\\"e\\\" f\"", S)]));
        // A Rust lifetime is no string; characters are.
        assert_eq!(marked("rs", "fn f<'a>(x: &'a str) -> char { '\\n' }"), of(&[("fn", K), ("'\\n'", S)]));
        assert_eq!(marked("rs", "let c = '\u{1F600}';"), of(&[("let", K), ("'\u{1F600}'", S)]));
        // A comment marker inside a string is part of it.
        assert_eq!(marked("sh", "echo '# not' # yes"), of(&[("echo", K), ("'# not'", S), ("# yes", C)]));
    }

    #[test]
    fn emoji_dotted_capitals_and_crlf_stay_whole() {
        assert_eq!(marked("py", "x = \"\u{130}stanbul \u{1F600}\" # \u{f1}"), of(&[("\"\u{130}stanbul \u{1F600}\"", S), ("# \u{f1}", C)]));
        // An escape just before a character of several bytes.
        assert_eq!(marked("rs", "\"\\\u{e9}\" 1"), of(&[("\"\\\u{e9}\"", S), ("1", N)]));
        // Words in other alphabets are words: no number inside them, no keyword.
        assert_eq!(marked("rs", "let \u{130}f1 = \u{5e9}\u{5dc}if;"), of(&[("let", K)]));
        // HTML text with an emoji and right-to-left letters between tags.
        assert_eq!(marked("html", "<b>\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{1F600}'</b>"), of(&[("<b>", K), ("</b>", K)]));
        // A line that still ends in \r (CRLF): the \r is plain, the comment stays a comment.
        assert_eq!(marked("rs", "let a = 1; // c\r\nfn"), of(&[("let", K), ("1", N), ("// c\r", C), ("fn", K)]));
    }

    #[test]
    fn rows_coloured_one_by_one_match_the_whole_line() {
        // However a line is cut into rows - through a keyword, a "/*", a string or an emoji - its
        // rows together are coloured as the whole line is.
        let text = "fn main() { let s = \"a /* b */ \u{1F600}\"; /* real\ncomment */ let n = 12345.678; // end\nr#'x' 'y' \u{130}";
        let whole: Vec<(usize, String, Kind)> = colour("rs", text, 10_000);
        for cols in 1..30 {
            assert_eq!(colour("rs", text, cols), whole, "rows of {cols}");
        }
        let py = "def f():\n    return '''a\n# not\n''' # yes 3.5";
        let whole = colour("py", py, 10_000);
        for cols in 1..12 {
            assert_eq!(colour("py", py, cols), whole, "rows of {cols}");
        }
    }

    #[test]
    fn a_huge_single_line_colours_only_the_rows_drawn_and_quickly() {
        // A minified file: 200 KB on one line, cut into rows of 100.
        let mut line = String::new();
        while line.len() < 200 * 1024 {
            line.push_str("function a(b){return \"x/*y\"+b*2.5}/*c*/var d='e';");
        }
        let lines = vec![line];
        let rows = rows_of(&lines, 100);
        let start = std::time::Instant::now();
        let mut painter = Painter::new(lang_for("js").unwrap());
        let mut runs = Vec::new();
        // The last rows first (as when scrolled straight to the end), then each again.
        for _ in 0..3 {
            for n in rows.len() - 40..rows.len() {
                let (_, a, b) = rows[n];
                painter.runs(n, |k| rows.get(k).copied(), &lines, a, b, &mut runs);
                assert!(!runs.is_empty() && runs[0].0 == a && runs.last().unwrap().1 == b);
            }
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "took {:?}", start.elapsed());
        // What it keeps is a few bytes a row.
        assert_eq!(painter.marks.len(), rows.len());
        assert!(std::mem::size_of::<Mark>() <= 8);
        // One huge string on one line, never closed: all of it a string.
        let lines = vec![format!("x = \"{}", "\u{1F600}a".repeat(50_000))];
        let rows = rows_of(&lines, 80);
        let mut painter = Painter::new(lang_for("json").unwrap());
        let last = rows.len() - 1;
        painter.runs(last, |k| rows.get(k).copied(), &lines, rows[last].1, rows[last].2, &mut runs);
        assert_eq!(runs.iter().map(|r| r.2).collect::<Vec<_>>(), [Kind::String]);
    }

    #[test]
    fn empty_text_and_empty_lines() {
        assert!(marked("rs", "").is_empty());
        assert_eq!(marked("c", "/*\n\n\n*/ int"), of(&[("/*", C), ("*/", C), ("int", K)]));
    }

    #[test]
    fn every_skin_gets_colours_that_differ_from_the_plain_text() {
        for name in ["amd-radeon", "amd-radeon-light", "nvidia", "nvidia-light", "chrome", "gunmetal"] {
            let theme = crate::skin::built_in_theme(name);
            let plain = theme.text;
            assert_eq!(color(&theme, Kind::Plain, plain), plain);
            for kind in [Kind::Comment, Kind::String, Kind::Keyword] {
                assert_ne!(color(&theme, kind, plain), plain, "{name}: {kind:?}");
            }
        }
    }
}
