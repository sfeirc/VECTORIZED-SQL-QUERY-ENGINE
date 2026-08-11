use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Select,
    From,
    Where,
    Group,
    By,
    Order,
    Limit,
    Inner,
    Join,
    On,
    As,
    And,
    Or,
    Not,
    Asc,
    Desc,
    Explain,
    Physical,
    Count,
    Sum,
    Avg,
    Min,
    Max,
    True,
    False,
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Keyword(Keyword),
    Ident(String),
    Number(String),
    String(String),
    Comma,
    Dot,
    Star,
    LParen,
    RParen,
    Semicolon,
    Plus,
    Minus,
    Slash,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub position: usize,
}

fn keyword(value: &str) -> Option<Keyword> {
    Some(match value.to_ascii_uppercase().as_str() {
        "SELECT" => Keyword::Select,
        "FROM" => Keyword::From,
        "WHERE" => Keyword::Where,
        "GROUP" => Keyword::Group,
        "BY" => Keyword::By,
        "ORDER" => Keyword::Order,
        "LIMIT" => Keyword::Limit,
        "INNER" => Keyword::Inner,
        "JOIN" => Keyword::Join,
        "ON" => Keyword::On,
        "AS" => Keyword::As,
        "AND" => Keyword::And,
        "OR" => Keyword::Or,
        "NOT" => Keyword::Not,
        "ASC" => Keyword::Asc,
        "DESC" => Keyword::Desc,
        "EXPLAIN" => Keyword::Explain,
        "PHYSICAL" => Keyword::Physical,
        "COUNT" => Keyword::Count,
        "SUM" => Keyword::Sum,
        "AVG" => Keyword::Avg,
        "MIN" => Keyword::Min,
        "MAX" => Keyword::Max,
        "TRUE" => Keyword::True,
        "FALSE" => Keyword::False,
        "NULL" => Keyword::Null,
        _ => return None,
    })
}

pub fn tokenize(input: &str) -> Result<Vec<Token>> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b',' => {
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    position: i,
                });
                i += 1;
            }
            b'.' => {
                tokens.push(Token {
                    kind: TokenKind::Dot,
                    position: i,
                });
                i += 1;
            }
            b'*' => {
                tokens.push(Token {
                    kind: TokenKind::Star,
                    position: i,
                });
                i += 1;
            }
            b'(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    position: i,
                });
                i += 1;
            }
            b')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    position: i,
                });
                i += 1;
            }
            b';' => {
                tokens.push(Token {
                    kind: TokenKind::Semicolon,
                    position: i,
                });
                i += 1;
            }
            b'+' => {
                tokens.push(Token {
                    kind: TokenKind::Plus,
                    position: i,
                });
                i += 1;
            }
            b'-' => {
                tokens.push(Token {
                    kind: TokenKind::Minus,
                    position: i,
                });
                i += 1;
            }
            b'/' => {
                tokens.push(Token {
                    kind: TokenKind::Slash,
                    position: i,
                });
                i += 1;
            }
            b'=' => {
                tokens.push(Token {
                    kind: TokenKind::Eq,
                    position: i,
                });
                i += 1;
            }
            b'!' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token {
                    kind: TokenKind::NotEq,
                    position: i,
                });
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token {
                    kind: TokenKind::LtEq,
                    position: i,
                });
                i += 2;
            }
            b'>' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token {
                    kind: TokenKind::GtEq,
                    position: i,
                });
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'>') => {
                tokens.push(Token {
                    kind: TokenKind::NotEq,
                    position: i,
                });
                i += 2;
            }
            b'<' => {
                tokens.push(Token {
                    kind: TokenKind::Lt,
                    position: i,
                });
                i += 1;
            }
            b'>' => {
                tokens.push(Token {
                    kind: TokenKind::Gt,
                    position: i,
                });
                i += 1;
            }
            b'\'' => {
                let start = i;
                i += 1;
                let mut value = String::new();
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            value.push('\'');
                            i += 2;
                        } else {
                            i += 1;
                            closed = true;
                            break;
                        }
                    } else {
                        let ch = input[i..].chars().next().expect("valid UTF-8 boundary");
                        value.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if !closed {
                    return Err(Error::Lex {
                        position: start,
                        message: "unterminated string literal".into(),
                    });
                }
                tokens.push(Token {
                    kind: TokenKind::String(value),
                    position: start,
                });
            }
            b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::Number(input[start..i].to_string()),
                    position: start,
                });
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let value = &input[start..i];
                let kind = keyword(value)
                    .map(TokenKind::Keyword)
                    .unwrap_or_else(|| TokenKind::Ident(value.to_string()));
                tokens.push(Token {
                    kind,
                    position: start,
                });
            }
            other => {
                return Err(Error::Lex {
                    position: i,
                    message: format!("unsupported character {:?}", char::from(other)),
                });
            }
        }
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        position: input.len(),
    });
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_keywords_case_insensitively() {
        let tokens = tokenize("select A FROM t").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Select));
        assert_eq!(tokens[1].kind, TokenKind::Ident("A".into()));
    }

    #[test]
    fn tokenizes_operators_comments_and_escaped_strings() {
        let tokens = tokenize("x <= 2 AND y != 'it''s' -- ignored\n").unwrap();
        assert!(tokens.iter().any(|t| t.kind == TokenKind::LtEq));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::NotEq));
        assert!(
            tokens
                .iter()
                .any(|t| t.kind == TokenKind::String("it's".into()))
        );
    }

    #[test]
    fn reports_unterminated_string_position() {
        assert!(matches!(
            tokenize("'bad"),
            Err(Error::Lex { position: 0, .. })
        ));
    }
}
