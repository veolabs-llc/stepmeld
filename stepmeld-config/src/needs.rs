//! What a deployment says each program's work needs, computed from one
//! request before any Performer is asked (docs/PLACEMENT.md §7):
//! `[programs.<name>.needs]`, one formula per need.
//!
//! ```toml
//! [programs.typeset-program.needs]
//! # ten pages a GiB, and two to start; the Input `book` carries `pages`
//! memory_gib = "ceil(inputs.book.pages / 10) + 2"
//! gpus = 1                                   # not a formula: as it is
//! capacity = "parameters.capacity ?? 'on-demand'"
//! ```
//!
//! A formula reads the request: `inputs.<name>` and `parameters.<name>`
//! (`parameters['page-size']` for a name with a dash in it), and `needs`,
//! the StepDefinition's own. It has numbers, strings in quotes,
//! `+ - * / ^`, comparisons, `and`, `or`, `not`, and these functions:
//! `min`, `max`, `ceil`, `floor`, `round`, `if(test, then, else)`,
//! `count(list)` and `sum(list, item => formula)`. `a ?? b` is `b`
//! wherever `a` cannot be computed (a field the Value does not carry, say).
//!
//! The engine never reads a Value; a deployment that sizes its work by
//! one says here which fields it reads, at the edge (Max, 2026-10-21).

use serde_json::Value;
use std::collections::BTreeMap;

/// A formula, read.
#[derive(Debug, Clone, PartialEq)]
pub enum Formula {
    Literal(Value),
    /// A name: a root (`inputs`, `parameters`, `needs`) or an item of
    /// `sum`.
    Name(String),
    Field(Box<Formula>, Box<Formula>),
    Unary(String, Box<Formula>),
    Binary(String, Box<Formula>, Box<Formula>),
    Call(String, Vec<Formula>),
    /// `item => formula`, only as `sum`'s second argument.
    Lambda(String, Box<Formula>),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Str(String),
    Ident(String),
    Op(String),
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                i += 1;
                if i < chars.len() && (chars[i] == '-' || chars[i] == '+') {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let s: String = chars[start..i].iter().filter(|c| **c != '_').collect();
            out.push(Token::Num(s.parse().map_err(|_| format!("{s:?} is not a number"))?));
        } else if c == '\'' || c == '"' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += 1;
            }
            if i == chars.len() {
                return Err("a string is not closed".into());
            }
            out.push(Token::Str(chars[start..i].iter().collect()));
            i += 1;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Token::Ident(chars[start..i].iter().collect()));
        } else {
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            if ["<=", ">=", "==", "!=", "??", "=>"].contains(&two.as_str()) {
                out.push(Token::Op(two));
                i += 2;
            } else if "+-*/^<>().,[]".contains(c) {
                out.push(Token::Op(c.to_string()));
                i += 1;
            } else {
                return Err(format!("{c:?} is not part of a formula"));
            }
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Token::Op(o)) if o == op)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(w)) if w == word)
    }

    fn expect(&mut self, op: &str) -> Result<(), String> {
        if self.is_op(op) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected {op:?}"))
        }
    }

    fn formula(&mut self) -> Result<Formula, String> {
        let left = self.or()?;
        if self.is_op("??") {
            self.at += 1;
            let right = self.formula()?;
            return Ok(Formula::Binary("??".into(), Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn or(&mut self) -> Result<Formula, String> {
        let mut left = self.and()?;
        while self.is_word("or") {
            self.at += 1;
            left = Formula::Binary("or".into(), Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Formula, String> {
        let mut left = self.compare()?;
        while self.is_word("and") {
            self.at += 1;
            left = Formula::Binary("and".into(), Box::new(left), Box::new(self.compare()?));
        }
        Ok(left)
    }

    fn compare(&mut self) -> Result<Formula, String> {
        let left = self.sum()?;
        for op in ["<", "<=", ">", ">=", "==", "!="] {
            if self.is_op(op) {
                self.at += 1;
                return Ok(Formula::Binary(op.into(), Box::new(left), Box::new(self.sum()?)));
            }
        }
        Ok(left)
    }

    fn sum(&mut self) -> Result<Formula, String> {
        let mut left = self.product()?;
        while self.is_op("+") || self.is_op("-") {
            let op = if self.is_op("+") { "+" } else { "-" };
            self.at += 1;
            left = Formula::Binary(op.into(), Box::new(left), Box::new(self.product()?));
        }
        Ok(left)
    }

    fn product(&mut self) -> Result<Formula, String> {
        let mut left = self.unary()?;
        while self.is_op("*") || self.is_op("/") {
            let op = if self.is_op("*") { "*" } else { "/" };
            self.at += 1;
            left = Formula::Binary(op.into(), Box::new(left), Box::new(self.unary()?));
        }
        Ok(left)
    }

    /// `^` binds tighter than a sign before it (`-2 ^ 2` is -4) and
    /// groups from the right.
    fn power(&mut self) -> Result<Formula, String> {
        let base = self.postfix()?;
        if self.is_op("^") {
            self.at += 1;
            return Ok(Formula::Binary("^".into(), Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<Formula, String> {
        if self.is_op("-") {
            self.at += 1;
            return Ok(Formula::Unary("-".into(), Box::new(self.unary()?)));
        }
        if self.is_word("not") {
            self.at += 1;
            return Ok(Formula::Unary("not".into(), Box::new(self.unary()?)));
        }
        self.power()
    }

    fn postfix(&mut self) -> Result<Formula, String> {
        let mut f = self.primary()?;
        loop {
            if self.is_op(".") {
                self.at += 1;
                match self.peek().cloned() {
                    Some(Token::Ident(name)) => {
                        self.at += 1;
                        f = Formula::Field(Box::new(f), Box::new(Formula::Literal(Value::from(name))));
                    }
                    _ => return Err("a name after `.`".into()),
                }
            } else if self.is_op("[") {
                self.at += 1;
                let key = self.formula()?;
                self.expect("]")?;
                f = Formula::Field(Box::new(f), Box::new(key));
            } else {
                return Ok(f);
            }
        }
    }

    fn primary(&mut self) -> Result<Formula, String> {
        match self.peek().cloned() {
            Some(Token::Num(n)) => {
                self.at += 1;
                Ok(Formula::Literal(number(n)))
            }
            Some(Token::Str(s)) => {
                self.at += 1;
                Ok(Formula::Literal(Value::from(s)))
            }
            Some(Token::Ident(word)) => {
                self.at += 1;
                match word.as_str() {
                    "true" => return Ok(Formula::Literal(Value::Bool(true))),
                    "false" => return Ok(Formula::Literal(Value::Bool(false))),
                    _ => {}
                }
                if self.is_op("(") {
                    self.at += 1;
                    let mut args = Vec::new();
                    if !self.is_op(")") {
                        loop {
                            args.push(self.argument()?);
                            if self.is_op(",") {
                                self.at += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(")")?;
                    return Ok(Formula::Call(word, args));
                }
                Ok(Formula::Name(word))
            }
            Some(Token::Op(o)) if o == "(" => {
                self.at += 1;
                let f = self.formula()?;
                self.expect(")")?;
                Ok(f)
            }
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("the formula ends too soon".into()),
        }
    }

    fn argument(&mut self) -> Result<Formula, String> {
        if let (Some(Token::Ident(name)), Some(Token::Op(arrow))) = (self.tokens.get(self.at).cloned(), self.tokens.get(self.at + 1)) {
            if arrow == "=>" {
                self.at += 2;
                return Ok(Formula::Lambda(name, Box::new(self.formula()?)));
            }
        }
        self.formula()
    }
}

/// A number as JSON: whole numbers as integers.
fn number(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        Value::from(n)
    }
}

impl Formula {
    /// Read a formula.
    pub fn parse(text: &str) -> Result<Formula, String> {
        let mut p = Parser { tokens: tokens(text)?, at: 0 };
        let f = p.formula()?;
        if p.at < p.tokens.len() {
            return Err(format!("unexpected {:?}", p.tokens[p.at]));
        }
        Ok(f)
    }

    /// Its value over `scope` (a name to a Value).
    pub fn eval(&self, scope: &BTreeMap<String, Value>) -> Result<Value, String> {
        let num = |f: &Formula| -> Result<f64, String> {
            let v = f.eval(scope)?;
            v.as_f64().ok_or_else(|| format!("{} is not a number", short(&v)))
        };
        let truth = |f: &Formula| -> Result<bool, String> {
            match f.eval(scope)? {
                Value::Bool(b) => Ok(b),
                v => Err(format!("{} is not true or false", short(&v))),
            }
        };
        match self {
            Formula::Literal(v) => Ok(v.clone()),
            Formula::Name(n) => scope.get(n).cloned().ok_or_else(|| format!("nothing is called {n}")),
            Formula::Field(of, key) => {
                let of_v = of.eval(scope)?;
                let key_v = key.eval(scope)?;
                let got = match (&of_v, &key_v) {
                    (Value::Object(m), Value::String(k)) => m.get(k).cloned(),
                    (Value::Array(a), k) if k.as_u64().is_some() => a.get(k.as_u64().unwrap() as usize).cloned(),
                    _ => None,
                };
                got.ok_or_else(|| format!("{} is not in {}", short(&key_v), of.describe()))
            }
            Formula::Unary(op, f) => match op.as_str() {
                "-" => Ok(number(-num(f)?)),
                _ => Ok(Value::Bool(!truth(f)?)),
            },
            Formula::Binary(op, a, b) => match op.as_str() {
                "??" => a.eval(scope).or_else(|_| b.eval(scope)),
                "and" => Ok(Value::Bool(truth(a)? && truth(b)?)),
                "or" => Ok(Value::Bool(truth(a)? || truth(b)?)),
                "==" => Ok(Value::Bool(a.eval(scope)? == b.eval(scope)?)),
                "!=" => Ok(Value::Bool(a.eval(scope)? != b.eval(scope)?)),
                _ => {
                    let (x, y) = (num(a)?, num(b)?);
                    Ok(match op.as_str() {
                        "+" => number(x + y),
                        "-" => number(x - y),
                        "*" => number(x * y),
                        "/" if y == 0.0 => return Err("a division by zero".into()),
                        "/" => number(x / y),
                        "^" => number(x.powf(y)),
                        "<" => Value::Bool(x < y),
                        "<=" => Value::Bool(x <= y),
                        ">" => Value::Bool(x > y),
                        _ => Value::Bool(x >= y),
                    })
                }
            },
            Formula::Lambda(..) => Err("`item => formula` is only for sum".into()),
            Formula::Call(name, args) => {
                let arity = |n: usize| if args.len() == n { Ok(()) } else { Err(format!("{name} takes {n} argument{}", if n == 1 { "" } else { "s" })) };
                match name.as_str() {
                    "min" | "max" => {
                        if args.is_empty() {
                            return Err(format!("{name} of nothing"));
                        }
                        let mut out = num(&args[0])?;
                        for a in &args[1..] {
                            let v = num(a)?;
                            out = if name == "min" { out.min(v) } else { out.max(v) };
                        }
                        Ok(number(out))
                    }
                    "ceil" | "floor" | "round" => {
                        arity(1)?;
                        let v = num(&args[0])?;
                        Ok(number(match name.as_str() {
                            "ceil" => v.ceil(),
                            "floor" => v.floor(),
                            _ => v.round(),
                        }))
                    }
                    "if" => {
                        arity(3)?;
                        if truth(&args[0])? {
                            args[1].eval(scope)
                        } else {
                            args[2].eval(scope)
                        }
                    }
                    "count" => {
                        arity(1)?;
                        match args[0].eval(scope)? {
                            Value::Array(a) => Ok(Value::from(a.len())),
                            v => Err(format!("{} is not a list", short(&v))),
                        }
                    }
                    "sum" => {
                        arity(2)?;
                        let Value::Array(items) = args[0].eval(scope)? else {
                            return Err(format!("{} is not a list", args[0].describe()));
                        };
                        let Formula::Lambda(item, body) = &args[1] else {
                            return Err("sum's second argument is `item => formula`".into());
                        };
                        let mut inner = scope.clone();
                        let mut total = 0.0;
                        for v in items {
                            inner.insert(item.clone(), v);
                            total += body.eval(&inner)?.as_f64().ok_or("sum adds numbers")?;
                        }
                        Ok(number(total))
                    }
                    _ => Err(format!("there is no function {name}")),
                }
            }
        }
    }

    /// How a formula reads, for words about it: `inputs.model`.
    fn describe(&self) -> String {
        match self {
            Formula::Name(n) => n.clone(),
            Formula::Field(of, key) => match key.as_ref() {
                Formula::Literal(Value::String(k)) if !k.contains('-') => format!("{}.{k}", of.describe()),
                Formula::Literal(k) => format!("{}[{k}]", of.describe()),
                _ => format!("{}[…]", of.describe()),
            },
            _ => "the value".into(),
        }
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 40 {
        format!("{}…", &s[..s.char_indices().nth(40).map_or(s.len(), |(i, _)| i)])
    } else {
        s
    }
}

/// One program's needs: each need's formula, or its value as it is.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Needs(pub BTreeMap<String, Formula>);

impl Needs {
    /// `[programs.<name>.needs]`: a string is a formula; anything else
    /// is taken as it is.
    pub fn from_table(table: &toml::Table) -> Result<Needs, String> {
        let mut out = BTreeMap::new();
        for (k, v) in table {
            let f = match v {
                toml::Value::String(s) => Formula::parse(s).map_err(|e| format!("needs.{k}: {e}"))?,
                other => Formula::Literal(serde_json::to_value(other).map_err(|e| format!("needs.{k}: {e}"))?),
            };
            out.insert(k.clone(), f);
        }
        Ok(Needs(out))
    }

    /// What a request needs by these formulas, over its Inputs, its
    /// Parameters and the StepDefinition's own `needs`.
    pub fn eval(&self, request: &stepmeld_core::engine::StartRequest) -> Result<BTreeMap<String, Value>, String> {
        let obj = |m: &BTreeMap<String, Value>| Value::Object(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let scope = BTreeMap::from([("inputs".to_string(), obj(&request.inputs)), ("parameters".to_string(), obj(&request.parameters)), ("needs".to_string(), obj(&request.needs))]);
        self.0.iter().map(|(k, f)| f.eval(&scope).map(|v| (k.clone(), v)).map_err(|e| format!("{k}: {e}"))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eval(text: &str, scope: Value) -> Result<Value, String> {
        let scope: BTreeMap<String, Value> = serde_json::from_value(scope).unwrap();
        Formula::parse(text)?.eval(&scope)
    }

    #[test]
    fn arithmetic_reads_as_written() {
        assert_eq!(eval("1 + 2 * 3 ^ 2", json!({})), Ok(json!(19)));
        assert_eq!(eval("(1 + 2) * 3", json!({})), Ok(json!(9)));
        assert_eq!(eval("-2 ^ 2", json!({})), Ok(json!(-4)), "the power binds first");
        assert_eq!(eval("2 ^ 3 ^ 2", json!({})), Ok(json!(512)), "and from the right");
        assert_eq!(eval("7 / 2", json!({})), Ok(json!(3.5)));
        assert_eq!(eval("ceil(7 / 2) + floor(2.9) + round(0.5)", json!({})), Ok(json!(7)));
        assert_eq!(eval("min(3, 1, 2) + max(4, 9)", json!({})), Ok(json!(10)));
        assert_eq!(eval("1_000 * 1e3", json!({})), Ok(json!(1000000)));
        assert_eq!(eval("1 / 0", json!({})), Err("a division by zero".into()));
    }

    #[test]
    fn fields_are_read_from_the_request() {
        let scope = json!({"inputs": {"model": {"pages": 40}}, "parameters": {"page-size": "a4", "copies": 3}});
        assert_eq!(eval("inputs.model.pages / 10", scope.clone()), Ok(json!(4)));
        assert_eq!(eval("parameters['page-size']", scope.clone()), Ok(json!("a4")));
        assert_eq!(eval("if(parameters.copies > 2 and true, 'many', 'few')", scope.clone()), Ok(json!("many")));
        assert_eq!(eval("inputs.model.words", scope.clone()), Err("\"words\" is not in inputs.model".into()));
        assert_eq!(eval("inputs.model.words ?? inputs.model.pages * 300", scope.clone()), Ok(json!(12000)), "?? stands in for what is missing");
        assert_eq!(eval("parameters.capacity ?? 'on-demand'", scope), Ok(json!("on-demand")));
    }

    #[test]
    fn a_sum_runs_over_a_list() {
        let scope = json!({"inputs": {"set": {"parts": [{"w": 2, "n": 3}, {"w": 5, "n": 1}]}}});
        assert_eq!(eval("sum(inputs.set.parts, p => p.w * p.n)", scope.clone()), Ok(json!(11)));
        assert_eq!(eval("count(inputs.set.parts)", scope.clone()), Ok(json!(2)));
        assert_eq!(eval("sum(inputs.set.parts, p => p.h)", scope).unwrap_err(), "\"h\" is not in p");
    }

    #[test]
    fn a_formula_that_does_not_read_says_where() {
        assert!(Formula::parse("1 +").unwrap_err().contains("too soon"));
        assert!(Formula::parse("1 $ 2").unwrap_err().contains("not part of a formula"));
        assert!(Formula::parse("(1").unwrap_err().contains("expected"));
        assert_eq!(eval("nope(1)", json!({})), Err("there is no function nope".into()));
        assert_eq!(eval("x", json!({})), Err("nothing is called x".into()));
    }

    #[test]
    fn a_program_s_needs_are_computed_from_its_request() {
        let table: toml::Table = toml::from_str("memory_gib = \"inputs.book.pages / 10 + needs.base\"\ngpus = 1\ncapacity = \"parameters.capacity ?? 'on-demand'\"").unwrap();
        let needs = Needs::from_table(&table).unwrap();
        let request: stepmeld_core::engine::StartRequest = serde_json::from_value(json!({
            "workflow": "wf", "step": "s", "run": "r", "definition": {"name": "typeset", "version": 1},
            "needs": {"base": 2}, "inputs": {"book": {"pages": 80}}, "parameters": {}, "outputs": []
        }))
        .unwrap();
        assert_eq!(needs.eval(&request), Ok(BTreeMap::from([("capacity".into(), json!("on-demand")), ("gpus".into(), json!(1)), ("memory_gib".into(), json!(10))])));
        let bad = Needs::from_table(&toml::from_str("memory_gib = \"inputs.book.words\"").unwrap()).unwrap();
        assert_eq!(bad.eval(&request), Err("memory_gib: \"words\" is not in inputs.book".into()));
        assert!(Needs::from_table(&toml::from_str("memory_gib = \"1 +\"").unwrap()).unwrap_err().starts_with("needs.memory_gib:"));
    }
}
