use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Lex { position: usize, message: String },
    Parse { position: usize, message: String },
    Bind(String),
    Plan(String),
    Execution(String),
    Storage(String),
    Io(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lex { position, message } => write!(f, "lex error at byte {position}: {message}"),
            Self::Parse { position, message } => {
                write!(f, "parse error at byte {position}: {message}")
            }
            Self::Bind(message) => write!(f, "binding error: {message}"),
            Self::Plan(message) => write!(f, "planning error: {message}"),
            Self::Execution(message) => write!(f, "execution error: {message}"),
            Self::Storage(message) => write!(f, "storage error: {message}"),
            Self::Io(message) => write!(f, "I/O error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
