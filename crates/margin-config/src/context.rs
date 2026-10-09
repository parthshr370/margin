//! The context tree and its expressions (CU-KEY-3, CU-KEY-4), shared by keymap `context` and
//! registry `when`.
//!
//! The UI builds the focused stack, outermost first (`App > Reader > Page`). Each node has
//! a name, attributes (`format=pdf`) and flags (`focus_mode`). An expression is evaluated at one
//! node, as in Zed: `Name` and `flag` match that node, `attr == v` reads that node's attribute,
//! and `A > B` holds where `B` holds and `A` holds at some node above. A binding's depth is
//! 1 + the deepest node where its context holds; no context is depth 0 (Zed's rule).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Longest expression accepted, in bytes (STD-RS-05).
pub const MAX_EXPR_BYTES: usize = 1024;

/// Deepest nesting of `!`, `()` and operators (STD-RS-06).
const MAX_EXPR_DEPTH: usize = 32;

/// Most nodes in a stack.
pub const MAX_STACK_NODES: usize = 32;

/// One node of the focused context stack.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContextNode {
    /// Node name: `App`, `Reader`, `TextInput`…
    pub name: String,
    /// Attributes such as `format=pdf`. Names are unique across the tree.
    #[serde(default)]
    pub attrs: BTreeMap<String, String>,
    /// Flags such as `focus_mode`.
    #[serde(default)]
    pub flags: Vec<String>,
}

/// A parsed context expression, in the JSON form the frontend evaluates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextExpr {
    /// A node name or a flag of the node.
    Name {
        /// The name or flag.
        name: String,
    },
    /// `attr == value` on the node.
    Eq {
        /// Attribute name.
        attr: String,
        /// Expected value.
        value: String,
    },
    /// `attr != value` on the node (true when the attribute is absent).
    Ne {
        /// Attribute name.
        attr: String,
        /// Excluded value.
        value: String,
    },
    /// `!expr`.
    Not {
        /// The negated expression.
        expr: Box<ContextExpr>,
    },
    /// `left && right`.
    And {
        /// Left operand.
        left: Box<ContextExpr>,
        /// Right operand.
        right: Box<ContextExpr>,
    },
    /// `left || right`.
    Or {
        /// Left operand.
        left: Box<ContextExpr>,
        /// Right operand.
        right: Box<ContextExpr>,
    },
    /// `ancestor > expr`: `expr` holds here and `ancestor` holds at a node above.
    Descendant {
        /// Must hold at some node above.
        ancestor: Box<ContextExpr>,
        /// Must hold at this node.
        expr: Box<ContextExpr>,
    },
}

impl ContextExpr {
    /// Parses a CU-KEY-4 expression. Precedence, loosest first, as in Zed: `>`, `||`, `&&`,
    /// `==`/`!=`, `!`.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_EXPR_BYTES {
            return Err(Error::Context { expr: truncate(text), reason: "longer than 1024 bytes" });
        }
        let tokens = tokenize(text)?;
        let mut parser = Parser { tokens: &tokens, at: 0, text };
        let expr = parser.expr(0, 0)?;
        match parser.peek() {
            None => Ok(expr),
            Some(_) => Err(parser.error("unexpected token after the expression")),
        }
    }

    /// Whether the expression names `name` anywhere (used for `TextInput`, CU-KEY-5).
    pub fn mentions(&self, name: &str) -> bool {
        match self {
            Self::Name { name: n } => n == name,
            Self::Eq { .. } | Self::Ne { .. } => false,
            Self::Not { expr } => expr.mentions(name),
            Self::And { left, right } | Self::Or { left, right } => left.mentions(name) || right.mentions(name),
            Self::Descendant { ancestor, expr } => ancestor.mentions(name) || expr.mentions(name),
        }
    }

    fn holds_at(&self, stack: &[ContextNode], at: usize) -> bool {
        let Some(node) = stack.get(at) else { return false };
        match self {
            Self::Name { name } => node.name == *name || node.flags.iter().any(|flag| flag == name),
            Self::Eq { attr, value } => node.attrs.get(attr) == Some(value),
            Self::Ne { attr, value } => node.attrs.get(attr) != Some(value),
            Self::Not { expr } => !expr.holds_at(stack, at),
            Self::And { left, right } => left.holds_at(stack, at) && right.holds_at(stack, at),
            Self::Or { left, right } => left.holds_at(stack, at) || right.holds_at(stack, at),
            Self::Descendant { ancestor, expr } => expr.holds_at(stack, at) && (0..at).any(|above| ancestor.holds_at(stack, above)),
        }
    }
}

/// 1 + the deepest stack index where `expr` holds; 0 for no context; `None` when it never holds.
pub fn depth_of(expr: Option<&ContextExpr>, stack: &[ContextNode]) -> Option<usize> {
    let Some(expr) = expr else { return Some(0) };
    (0..stack.len()).rev().find(|&at| expr.holds_at(stack, at)).map(|at| at.saturating_add(1))
}

/// Whether a `when` expression holds anywhere in the stack (`None` always holds).
pub fn holds(expr: Option<&ContextExpr>, stack: &[ContextNode]) -> bool {
    expr.is_none_or(|expr| (0..stack.len()).any(|at| expr.holds_at(stack, at)))
}

/// Parses the stack notation used by `margin-cli --context` and the keymap cases:
/// `App os=linux > Reader format=pdf layout=scroll focus_mode > Page`. Each node is a name,
/// then `attr=value` pairs and bare flags.
pub fn parse_stack(text: &str) -> Result<Vec<ContextNode>> {
    let mut stack = Vec::new();
    for part in text.split('>').map(str::trim).filter(|part| !part.is_empty()) {
        if stack.len() >= MAX_STACK_NODES {
            return Err(Error::Stack { stack: truncate(text), reason: "more than 32 nodes" });
        }
        let mut words = part.split_whitespace();
        let Some(name) = words.next().filter(|name| is_word(name)) else {
            return Err(Error::Stack { stack: truncate(text), reason: "a node needs a name" });
        };
        let mut node = ContextNode { name: name.to_owned(), ..ContextNode::default() };
        for word in words {
            match word.split_once('=') {
                Some((attr, value)) if is_word(attr) && is_value(value) => {
                    node.attrs.insert(attr.to_owned(), value.to_owned());
                }
                None if is_word(word) => node.flags.push(word.to_owned()),
                _ => return Err(Error::Stack { stack: truncate(text), reason: "attributes are name=value, flags are words" }),
            }
        }
        stack.push(node);
    }
    Ok(stack)
}

fn truncate(text: &str) -> String {
    text.chars().take(80).collect()
}

fn is_word(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Attribute values may also hold `.` and `-` (`tool == comment.ink`).
fn is_value(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    Not,
    And,
    Or,
    Eq,
    Ne,
    Child,
    Open,
    Close,
}

fn tokenize(text: &str) -> Result<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = text.trim_start();
    while let Some(c) = rest.chars().next() {
        let (token, len) = match c {
            '&' if rest.starts_with("&&") => (Token::And, 2),
            '|' if rest.starts_with("||") => (Token::Or, 2),
            '=' if rest.starts_with("==") => (Token::Eq, 2),
            '!' if rest.starts_with("!=") => (Token::Ne, 2),
            '!' => (Token::Not, 1),
            '>' => (Token::Child, 1),
            '(' => (Token::Open, 1),
            ')' => (Token::Close, 1),
            _ => {
                let len = rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))).unwrap_or(rest.len());
                let word = rest.get(..len).unwrap_or_default();
                if word.is_empty() {
                    return Err(Error::Context { expr: truncate(text), reason: "unexpected character" });
                }
                (Token::Word(word), len)
            }
        };
        tokens.push(token);
        rest = rest.get(len..).unwrap_or_default().trim_start();
    }
    Ok(tokens)
}

/// Binding power of each infix operator; higher binds tighter.
fn infix_power(token: &Token<'_>) -> Option<u8> {
    match token {
        Token::Child => Some(1),
        Token::Or => Some(2),
        Token::And => Some(3),
        Token::Eq | Token::Ne => Some(4),
        _ => None,
    }
}

/// Binding power of prefix `!`: tighter than every infix operator.
const NOT_POWER: u8 = 5;

struct Parser<'t, 'a> {
    tokens: &'t [Token<'a>],
    at: usize,
    text: &'a str,
}

impl<'a> Parser<'_, 'a> {
    fn peek(&self) -> Option<&Token<'a>> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token<'a>> {
        let token = self.tokens.get(self.at).cloned();
        self.at = self.at.saturating_add(1);
        token
    }

    fn error(&self, reason: &'static str) -> Error {
        Error::Context { expr: truncate(self.text), reason }
    }

    fn expr(&mut self, min_power: u8, depth: usize) -> Result<ContextExpr> {
        if depth > MAX_EXPR_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        let mut left = self.primary(depth)?;
        while let Some(power) = self.peek().and_then(infix_power) {
            if power < min_power {
                break;
            }
            let Some(op) = self.next() else { break };
            if matches!(op, Token::Eq | Token::Ne) {
                let ContextExpr::Name { name: attr } = left else { return Err(self.error("the left of == or != must be an attribute name")) };
                let Some(Token::Word(value)) = self.next() else { return Err(self.error("== and != need a value")) };
                left = if op == Token::Eq {
                    ContextExpr::Eq { attr, value: value.to_owned() }
                } else {
                    ContextExpr::Ne { attr, value: value.to_owned() }
                };
                continue;
            }
            let right = Box::new(self.expr(power.saturating_add(1), depth.saturating_add(1))?);
            let left_box = Box::new(left);
            left = match op {
                Token::Child => ContextExpr::Descendant { ancestor: left_box, expr: right },
                Token::Or => ContextExpr::Or { left: left_box, right },
                _ => ContextExpr::And { left: left_box, right },
            };
        }
        Ok(left)
    }

    fn primary(&mut self, depth: usize) -> Result<ContextExpr> {
        match self.next() {
            Some(Token::Word(word)) if is_word(word) => Ok(ContextExpr::Name { name: word.to_owned() }),
            Some(Token::Not) => Ok(ContextExpr::Not { expr: Box::new(self.expr(NOT_POWER, depth.saturating_add(1))?) }),
            Some(Token::Open) => {
                let inner = self.expr(0, depth.saturating_add(1))?;
                match self.next() {
                    Some(Token::Close) => Ok(inner),
                    _ => Err(self.error("missing )")),
                }
            }
            _ => Err(self.error("expected a name, ! or (")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(text: &str) -> Vec<ContextNode> {
        parse_stack(text).unwrap()
    }

    #[test]
    fn child_binds_loosest_as_in_zed() {
        let expr = ContextExpr::parse("Palette > TextInput || List").unwrap();
        assert!(matches!(expr, ContextExpr::Descendant { .. }));
        assert_eq!(depth_of(Some(&expr), &stack("App > Palette > List")), Some(3));
        assert_eq!(depth_of(Some(&expr), &stack("App > Reader > List")), None);
    }

    #[test]
    fn attributes_and_names_are_per_node() {
        let expr = ContextExpr::parse("Reader && format == pdf").unwrap();
        assert_eq!(depth_of(Some(&expr), &stack("App os=linux > Reader format=pdf > Page")), Some(2));
        assert_eq!(depth_of(Some(&expr), &stack("App > Reader format=epub > Page")), None);
        assert!(holds(Some(&ContextExpr::parse("!(format == epub)").unwrap()), &stack("App > Reader format=pdf")));
    }

    #[test]
    fn flags_match_like_names() {
        let expr = ContextExpr::parse("Reader && !focus_mode").unwrap();
        assert!(holds(Some(&expr), &stack("App > Reader")));
        assert!(!holds(Some(&expr), &stack("App > Reader focus_mode")));
    }

    #[test]
    fn values_may_hold_dots() {
        let expr = ContextExpr::parse("AnnotateMode && tool == comment.ink").unwrap();
        assert!(holds(Some(&expr), &stack("App > Reader > AnnotateMode tool=comment.ink")));
    }

    #[test]
    fn bad_expressions_are_errors() {
        for text in ["", "Reader &&", "(Reader", "== pdf", "Reader == ", "a ! b", "Reader $"] {
            assert!(ContextExpr::parse(text).is_err(), "{text:?}");
        }
        let deep = format!("{}Reader{}", "(".repeat(100), ")".repeat(100));
        assert!(ContextExpr::parse(&deep).is_err());
    }

    #[test]
    fn json_form_is_tagged() {
        let json = serde_json::to_value(ContextExpr::parse("!Reader").unwrap()).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "not", "expr": { "kind": "name", "name": "Reader" } }));
    }
}
