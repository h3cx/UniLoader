//! Small, lossless-in-content KiCad S-expression reader.
use anyhow::{Result, bail};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Atom(String),
    Str(String),
    List(Vec<Node>),
}

impl Node {
    pub fn head(&self) -> Option<&str> {
        self.children().first().and_then(Node::atom)
    }
    pub fn atom(&self) -> Option<&str> {
        match self {
            Self::Atom(s) | Self::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn children(&self) -> &[Node] {
        match self {
            Self::List(v) => v,
            _ => &[],
        }
    }
    pub fn children_mut(&mut self) -> &mut Vec<Node> {
        match self {
            Self::List(v) => v,
            _ => panic!("expected S-expression list"),
        }
    }
    pub fn field(&self, name: &str) -> Option<&Node> {
        self.children().iter().find(|n| n.head() == Some(name))
    }
    pub fn field_mut(&mut self, name: &str) -> Option<&mut Node> {
        self.children_mut()
            .iter_mut()
            .find(|n| n.head() == Some(name))
    }
    pub fn value(&self) -> Option<&str> {
        self.children().get(1).and_then(Node::atom)
    }
    pub fn string(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }
    pub fn symbol(s: impl Into<String>) -> Self {
        Self::Atom(s.into())
    }
    pub fn list(nodes: Vec<Node>) -> Self {
        Self::List(nodes)
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Atom(s) => f.write_str(s),
            Self::Str(s) => {
                f.write_str("\"")?;
                for c in s.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\r' => f.write_str("\\r")?,
                        '\t' => f.write_str("\\t")?,
                        _ => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Self::List(nodes) => {
                f.write_str("(")?;
                for (i, n) in nodes.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{n}")?;
                }
                f.write_str(")")
            }
        }
    }
}

pub fn parse(input: &str) -> Result<Node> {
    struct Reader<'a> {
        chars: std::iter::Peekable<std::str::Chars<'a>>,
        depth: usize,
    }
    impl Reader<'_> {
        fn spaces(&mut self) {
            while self
                .chars
                .peek()
                .is_some_and(|c| c.is_whitespace() || *c == '\u{feff}')
            {
                self.chars.next();
            }
        }
        fn read(&mut self) -> Result<Node> {
            self.spaces();
            match self.chars.next() {
                Some('(') => {
                    self.depth += 1;
                    if self.depth > 256 {
                        bail!("S-expression nesting exceeds 256 levels");
                    }
                    let mut v = Vec::new();
                    loop {
                        self.spaces();
                        match self.chars.peek() {
                            Some(')') => {
                                self.chars.next();
                                self.depth -= 1;
                                return Ok(Node::List(v));
                            }
                            None => bail!("unclosed S-expression list"),
                            _ => v.push(self.read()?),
                        }
                    }
                }
                Some(')') => bail!("unexpected closing parenthesis"),
                Some('"') => {
                    let mut s = String::new();
                    loop {
                        match self.chars.next() {
                            Some('"') => return Ok(Node::Str(s)),
                            Some('\\') => match self.chars.next() {
                                Some('n') => s.push('\n'),
                                Some('r') => s.push('\r'),
                                Some('t') => s.push('\t'),
                                Some('"') => s.push('"'),
                                Some('\\') => s.push('\\'),
                                Some(c) => {
                                    s.push('\\');
                                    s.push(c);
                                }
                                None => bail!("unterminated string escape"),
                            },
                            Some(c) => s.push(c),
                            None => bail!("unterminated quoted string"),
                        }
                    }
                }
                Some(c) => {
                    let mut s = String::from(c);
                    while self
                        .chars
                        .peek()
                        .is_some_and(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '"'))
                    {
                        s.push(self.chars.next().unwrap());
                    }
                    Ok(Node::Atom(s))
                }
                None => bail!("empty S-expression input"),
            }
        }
    }
    let mut reader = Reader {
        chars: input.chars().peekable(),
        depth: 0,
    };
    let result = reader.read()?;
    reader.spaces();
    if reader.chars.peek().is_some() {
        bail!("unexpected trailing content after S-expression");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_quoted_parentheses_and_escapes_round_trip() {
        let input = r#"(kicad_symbol_lib (version 20261101) (symbol "µ Ω (test)" (property "Description" "a \"quoted\" (thing) \\ end") (unknown_future (xyz "é"))))"#;
        let ast = parse(input).unwrap();
        assert_eq!(ast, parse(&ast.to_string()).unwrap());
        assert_eq!(ast.field("symbol").unwrap().value(), Some("µ Ω (test)"));
        assert_eq!(ast.field("version").unwrap().value(), Some("20261101"));
    }
    #[test]
    fn rejects_incomplete_and_trailing_input() {
        for bad in ["", "(a", "(a))", "(a) (b)", "(a \"unterminated)", ")"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn preserves_unknown_escape_and_fields() {
        let ast = parse(r#"(future "C:\models\part.step" (unknown 42))"#).unwrap();
        assert_eq!(ast, parse(&ast.to_string()).unwrap());
        assert!(ast.field("unknown").is_some());
    }
}
