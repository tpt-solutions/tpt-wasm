// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A reader for the subset of the `.wast` syntax the binary format tests use.
//!
//! This is a reader, not an interpreter: it turns a source file into a list of
//! top-level forms and hands back the module bytes of the ones this crate cares
//! about. Anything else becomes a [`Form::Skipped`] rather than a failure, so an
//! unrecognized directive in a vendored file cannot break the run.

/// A top-level form, reduced to what the harness acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Form {
    /// A module that must decode and validate.
    Module { line: usize, bytes: Vec<u8> },
    /// `assert_malformed`: the module must fail to decode.
    Malformed {
        line: usize,
        bytes: Vec<u8>,
        reason: String,
    },
    /// `assert_invalid`: the module must decode, then fail to validate.
    Invalid {
        line: usize,
        bytes: Vec<u8>,
        reason: String,
    },
    /// A directive this crate does not interpret.
    Skipped {
        line: usize,
        directive: &'static str,
    },
}

/// A syntax problem in a `.wast` source.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// A `(` with no matching `)`, or the other way round.
    Unbalanced(usize),
    /// A string literal that ends before its closing quote.
    UnterminatedString(usize),
    /// A `\x` escape in a string that is not two hex digits.
    BadEscape(usize),
    /// A character in a string literal above 0x7f.
    NonAsciiString(usize),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Unbalanced(line) => write!(f, "unbalanced parentheses at line {line}"),
            ParseError::UnterminatedString(line) => {
                write!(f, "unterminated string at line {line}")
            }
            ParseError::BadEscape(line) => write!(f, "bad string escape at line {line}"),
            ParseError::NonAsciiString(line) => {
                write!(f, "non-ASCII character in a string at line {line}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// A token: either a run of non-delimiter characters, or a string's bytes.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// An atom, already unescaped in the sense that `$name` and `0x1f` are
    /// returned as written; the harness never interprets them.
    Atom(String),
    /// The decoded bytes of a `"..."` literal.
    Str(Vec<u8>),
    Open,
    Close,
}

/// A token with the line it started on, for error reporting.
#[derive(Debug, Clone, PartialEq)]
struct Spanned {
    token: Token,
    line: usize,
}

impl Spanned {
    fn atom(&self) -> Option<&str> {
        match &self.token {
            Token::Atom(text) => Some(text),
            _ => None,
        }
    }
}

/// Split a source into tokens, dropping comments.
///
/// The two comment forms are `;;` to end of line and the nestable `(; ... ;)`.
fn tokenize(source: &str) -> Result<Vec<Spanned>, ParseError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1usize;
    let mut block_comment = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'\n' => {
                line += 1;
                index += 1;
            }
            b' ' | b'\t' | b'\r' => index += 1,
            b'(' if bytes.get(index + 1) == Some(&b';') => {
                block_comment += 1;
                index += 2;
            }
            b';' if bytes.get(index + 1) == Some(&b';') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'(' if block_comment > 0 => {
                block_comment += 1;
                index += 1;
            }
            b')' if block_comment > 0 => {
                block_comment -= 1;
                index += 1;
            }
            b'(' => {
                tokens.push(Spanned {
                    token: Token::Open,
                    line,
                });
                index += 1;
            }
            b')' => {
                tokens.push(Spanned {
                    token: Token::Close,
                    line,
                });
                index += 1;
            }
            b'"' => {
                let (bytes_out, next, next_line) = read_string(bytes, index, line)?;
                tokens.push(Spanned {
                    token: Token::Str(bytes_out),
                    line,
                });
                line = next_line;
                index = next;
            }
            _ => {
                let start = index;
                while index < bytes.len()
                    && !matches!(bytes[index], b'(' | b')' | b'"' | b' ' | b'\t' | b'\r' | b'\n')
                    // A `;;` starts a comment even mid-atom, so stop there too.
                    && !(bytes[index] == b';' && bytes.get(index + 1) == Some(&b';'))
                {
                    index += 1;
                }
                let text = std::str::from_utf8(&bytes[start..index])
                    .map_err(|_| ParseError::NonAsciiString(line))?;
                tokens.push(Spanned {
                    token: Token::Atom(text.to_string()),
                    line,
                });
            }
        }
    }
    if block_comment > 0 {
        return Err(ParseError::Unbalanced(line));
    }
    Ok(tokens)
}

/// Read a `"..."` literal starting at the opening quote.
///
/// Returns the decoded bytes, the index just past the closing quote, and the
/// line. The escapes are the ones the spec files use: `\XX` for a byte plus the
/// named ones. Anything else is an error rather than a guess, so a misread file
/// cannot silently become a different module.
fn read_string(
    bytes: &[u8],
    start: usize,
    line: usize,
) -> Result<(Vec<u8>, usize, usize), ParseError> {
    let mut out = Vec::new();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok((out, index + 1, line)),
            b'\n' => return Err(ParseError::UnterminatedString(line)),
            b'\\' => {
                let escape = bytes
                    .get(index + 1)
                    .copied()
                    .ok_or(ParseError::BadEscape(line))?;
                match escape {
                    b'n' => {
                        out.push(b'\n');
                        index += 2;
                    }
                    b't' => {
                        out.push(b'\t');
                        index += 2;
                    }
                    b'r' => {
                        out.push(b'\r');
                        index += 2;
                    }
                    b'\\' => {
                        out.push(b'\\');
                        index += 2;
                    }
                    b'"' => {
                        out.push(b'"');
                        index += 2;
                    }
                    b'\'' => {
                        out.push(b'\'');
                        index += 2;
                    }
                    _ => {
                        let high = hex(escape).ok_or(ParseError::BadEscape(line))?;
                        let low = bytes
                            .get(index + 2)
                            .copied()
                            .and_then(hex)
                            .ok_or(ParseError::BadEscape(line))?;
                        out.push(high << 4 | low);
                        index += 3;
                    }
                }
            }
            other => {
                if other > 0x7f {
                    return Err(ParseError::NonAsciiString(line));
                }
                out.push(other);
                index += 1;
            }
        }
    }
    Err(ParseError::UnterminatedString(line))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parse a source into its top-level forms.
pub fn parse(source: &str) -> Result<Vec<Form>, ParseError> {
    let tokens = tokenize(source)?;
    let mut forms = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match &tokens[index].token {
            Token::Close => return Err(ParseError::Unbalanced(tokens[index].line)),
            Token::Open => {
                let form = read_form(&tokens, &mut index)?;
                forms.push(form);
            }
            // A bare atom at the top level is not a form this harness reads.
            Token::Atom(_) | Token::Str(_) => index += 1,
        }
    }
    Ok(forms)
}

/// Read one parenthesized form, leaving `index` just past its closing paren.
fn read_form(tokens: &[Spanned], index: &mut usize) -> Result<Form, ParseError> {
    let line = tokens[*index].line;
    *index += 1; // the opening paren
    let head = tokens.get(*index).and_then(Spanned::atom).unwrap_or("");
    *index += 1;

    // The reason string an assertion ends with, read after the module.
    fn reason(tokens: &[Spanned], index: &mut usize) -> String {
        match tokens.get(*index).map(|token| &token.token) {
            Some(Token::Str(bytes)) => {
                *index += 1;
                String::from_utf8_lossy(bytes).into_owned()
            }
            _ => String::new(),
        }
    }

    let form = match head {
        "module" => match read_module(tokens, index)? {
            Some(bytes) => Form::Module { line, bytes },
            // A text-format module needs an assembler this crate does not have.
            None => Form::Skipped {
                line,
                directive: "module (text format)",
            },
        },
        "assert_malformed" => match read_assertion_module(tokens, index)? {
            Some(bytes) => {
                let reason = reason(tokens, index);
                Form::Malformed {
                    line,
                    bytes,
                    reason,
                }
            }
            None => Form::Skipped {
                line,
                directive: "assert_malformed (text format)",
            },
        },
        "assert_invalid" => match read_assertion_module(tokens, index)? {
            Some(bytes) => {
                let reason = reason(tokens, index);
                Form::Invalid {
                    line,
                    bytes,
                    reason,
                }
            }
            None => Form::Skipped {
                line,
                directive: "assert_invalid (text format)",
            },
        },
        other => Form::Skipped {
            line,
            directive: known_directive(other),
        },
    };
    skip_to_close(tokens, index);
    Ok(form)
}

/// Map a directive this crate does not interpret onto a name the report can show.
///
/// An unrecognized name is reported as `other` rather than being passed through,
/// so a skipped directive can never be mistaken for one that is covered.
fn known_directive(name: &str) -> &'static str {
    match name {
        "assert_return" => "assert_return",
        "assert_trap" => "assert_trap",
        "assert_exhaustion" => "assert_exhaustion",
        "assert_unlinkable" => "assert_unlinkable",
        "assert_uninstantiable" => "assert_uninstantiable",
        "register" => "register",
        "invoke" => "invoke",
        "assert_suspension" => "assert_suspension",
        _ => "other",
    }
}

/// Read the module of an assertion: a nested `(module binary ...)`.
///
/// Returns `None` for a text-format module, which needs an assembler this crate
/// does not have. The nested form's closing paren is consumed here, so the
/// caller is left just past it and can read the reason that follows.
fn read_assertion_module(
    tokens: &[Spanned],
    index: &mut usize,
) -> Result<Option<Vec<u8>>, ParseError> {
    if tokens.get(*index).map(|token| &token.token) != Some(&Token::Open) {
        return Err(ParseError::Unbalanced(
            tokens.get(*index).map_or(0, |token| token.line),
        ));
    }
    let line = tokens[*index].line;
    *index += 1;
    if tokens.get(*index).and_then(Spanned::atom) != Some("module") {
        return Err(ParseError::Unbalanced(line));
    }
    *index += 1;
    let bytes = read_module(tokens, index)?;
    skip_to_close(tokens, index);
    Ok(bytes)
}

/// Read the `binary` marker and the string chunks of a module form.
///
/// Returns `None` when the module is not in the binary form, so the caller can
/// report it as skipped instead of treating an unreadable module as an empty
/// one. An empty `(module binary "")` is a real case the spec asserts against,
/// and it must stay distinguishable from "not binary".
fn read_module(tokens: &[Spanned], index: &mut usize) -> Result<Option<Vec<u8>>, ParseError> {
    // An optional `$name` may sit between `module` and the format marker.
    if tokens
        .get(*index)
        .and_then(Spanned::atom)
        .is_some_and(|atom| atom.starts_with('$'))
    {
        *index += 1;
    }
    if tokens.get(*index).and_then(Spanned::atom) != Some("binary") {
        return Ok(None);
    }
    *index += 1;
    let mut bytes = Vec::new();
    while let Some(Spanned {
        token: Token::Str(chunk),
        ..
    }) = tokens.get(*index)
    {
        bytes.extend_from_slice(chunk);
        *index += 1;
    }
    Ok(Some(bytes))
}

/// Advance past the closing paren of the form being read.
fn skip_to_close(tokens: &[Spanned], index: &mut usize) {
    let mut depth = 1usize;
    while *index < tokens.len() && depth > 0 {
        match tokens[*index].token {
            Token::Open => depth += 1,
            Token::Close => depth -= 1,
            _ => {}
        }
        *index += 1;
    }
}
