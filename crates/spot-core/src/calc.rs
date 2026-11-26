//! Inline calculator: `2+3*4`, `(1.5^2) / 3`, `sqrt(2)`, `2pi`, `10!`, `= 42`.
//!
//! Only shown when the query is clearly arithmetic: it must parse completely and contain an
//! operator or a function call (so "2048" or "pi" alone stay app searches), or start with `=`.

/// A query that evaluated to a number.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// The expression as typed (trimmed, without a leading `=`).
    pub expr: String,
    pub value: f64,
    /// `value` formatted for display and copying.
    pub text: String,
}

pub fn evaluate(query: &str) -> Option<Answer> {
    let q = query.trim();
    let (forced, expr) = match q.strip_prefix('=') {
        Some(rest) => (true, rest.trim()),
        None => (false, q),
    };
    if expr.is_empty() || expr.len() > 200 {
        return None;
    }
    let tokens = tokenize(expr)?;
    let mut p = Parser { tokens: &tokens, pos: 0, operations: 0 };
    let value = p.expr()?;
    if p.pos != tokens.len() || !value.is_finite() || (!forced && p.operations == 0) {
        return None;
    }
    Some(Answer { expr: expr.to_owned(), value, text: format_number(value) })
}

/// Integers print as integers; other values with up to 12 significant digits, trailing zeros
/// trimmed; very large or small values in scientific notation.
pub fn format_number(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v }; // no "-0"
    let a = v.abs();
    if v.fract() == 0.0 && a < 1e15 {
        return format!("{v:.0}");
    }
    if a != 0.0 && !(1e-6..1e15).contains(&a) {
        let s = format!("{v:.11e}");
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        return format!("{mantissa}e{exp}");
    }
    let digits = (11 - a.log10().floor() as i32).clamp(0, 15) as usize;
    let s = format!("{v:.digits$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            // Exponent: 1e3, 2.5E-4 (but not "2e" followed by a letter: that's 2*e...)
            if i + 1 < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if chars[j] == '+' || chars[j] == '-' {
                    j += 1;
                }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    i = j;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text: String = chars[start..i].iter().filter(|c| **c != '_').collect();
            out.push(Tok::Num(text.parse().ok()?));
        } else if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric()) {
                i += 1;
            }
            out.push(Tok::Ident(chars[start..i].iter().collect::<String>().to_lowercase()));
        } else {
            out.push(match c {
                '+' | '-' | '*' | '/' | '%' | '^' | '!' => Tok::Op(c),
                '×' | '·' => Tok::Op('*'),
                '÷' => Tok::Op('/'),
                '−' => Tok::Op('-'),
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                ',' => Tok::Comma,
                _ => return None,
            });
            i += 1;
        }
    }
    Some(out)
}

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    /// Operators and function calls seen; a lone number or constant isn't a calculation.
    operations: u32,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    // expr := term (('+'|'-') term)*
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(&Tok::Op('+')) {
                v += self.term()?;
            } else if self.eat(&Tok::Op('-')) {
                v -= self.term()?;
            } else {
                return Some(v);
            }
            self.operations += 1;
        }
    }

    // term := unary (('*'|'/'|'%') unary | implicit-multiplication)*
    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat(&Tok::Op('*')) {
                v *= self.unary()?;
            } else if self.eat(&Tok::Op('/')) {
                v /= self.unary()?;
            } else if self.eat(&Tok::Op('%')) {
                v %= self.unary()?;
            } else if matches!(self.peek(), Some(Tok::LParen | Tok::Ident(_))) {
                // 2pi, 2(3+4), 3 sqrt(2)
                v *= self.power()?;
            } else {
                return Some(v);
            }
            self.operations += 1;
        }
    }

    // unary := ('-'|'+') unary | power
    fn unary(&mut self) -> Option<f64> {
        if self.eat(&Tok::Op('-')) {
            return Some(-self.unary()?);
        }
        if self.eat(&Tok::Op('+')) {
            return self.unary();
        }
        self.power()
    }

    // power := postfix ('^' unary)?    (right-associative; -2^2 = -4, 2^-1 = 0.5)
    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if self.eat(&Tok::Op('^')) {
            self.operations += 1;
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    // postfix := atom '!'*
    fn postfix(&mut self) -> Option<f64> {
        let mut v = self.atom()?;
        while self.eat(&Tok::Op('!')) {
            self.operations += 1;
            if v < 0.0 || v.fract() != 0.0 || v > 170.0 {
                return None;
            }
            v = (1..=v as u64).fold(1.0, |acc, k| acc * k as f64);
        }
        Some(v)
    }

    fn atom(&mut self) -> Option<f64> {
        match self.peek()?.clone() {
            Tok::Num(n) => {
                self.pos += 1;
                Some(n)
            }
            Tok::LParen => {
                self.pos += 1;
                let v = self.expr()?;
                self.eat(&Tok::RParen).then_some(v)
            }
            Tok::Ident(name) => {
                self.pos += 1;
                if let Some(c) = constant(&name) {
                    return Some(c);
                }
                // Function call; parentheses required so "log" alone isn't a calculation.
                if !self.eat(&Tok::LParen) {
                    return None;
                }
                let mut args = vec![self.expr()?];
                while self.eat(&Tok::Comma) {
                    args.push(self.expr()?);
                }
                if !self.eat(&Tok::RParen) {
                    return None;
                }
                self.operations += 1;
                function(&name, &args)
            }
            _ => None,
        }
    }
}

fn constant(name: &str) -> Option<f64> {
    Some(match name {
        "pi" | "π" => std::f64::consts::PI,
        "tau" | "τ" => std::f64::consts::TAU,
        "e" => std::f64::consts::E,
        _ => return None,
    })
}

fn function(name: &str, a: &[f64]) -> Option<f64> {
    let one = |f: fn(f64) -> f64| (a.len() == 1).then(|| f(a[0]));
    match name {
        "sqrt" => one(f64::sqrt),
        "cbrt" => one(f64::cbrt),
        "abs" => one(f64::abs),
        "exp" => one(f64::exp),
        "ln" => one(f64::ln),
        "log" | "log10" => match a {
            [x] => Some(x.log10()),
            [x, base] => Some(x.log(*base)),
            _ => None,
        },
        "log2" => one(f64::log2),
        "sin" => one(f64::sin),
        "cos" => one(f64::cos),
        "tan" => one(f64::tan),
        "asin" => one(f64::asin),
        "acos" => one(f64::acos),
        "atan" => one(f64::atan),
        "floor" => one(f64::floor),
        "ceil" => one(f64::ceil),
        "round" => one(f64::round),
        "min" => a.iter().copied().reduce(f64::min),
        "max" => a.iter().copied().reduce(f64::max),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(q: &str) -> Option<String> {
        evaluate(q).map(|a| a.text)
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(text("2+3*4").as_deref(), Some("14"));
        assert_eq!(text("(2+3)*4").as_deref(), Some("20"));
        assert_eq!(text("2^3^2").as_deref(), Some("512"));
        assert_eq!(text("-2^2").as_deref(), Some("-4"));
        assert_eq!(text("2^-1").as_deref(), Some("0.5"));
        assert_eq!(text("10 % 4").as_deref(), Some("2"));
        assert_eq!(text("7 ÷ 2 × 3").as_deref(), Some("10.5"));
        assert_eq!(text("1/3").as_deref(), Some("0.333333333333"));
        assert_eq!(text("0.1+0.2").as_deref(), Some("0.3"));
        assert_eq!(text("1_000 * 3").as_deref(), Some("3000"));
        assert_eq!(text("1.5e3 + 1").as_deref(), Some("1501"));
    }

    #[test]
    fn functions_constants_factorial_implicit_multiplication() {
        assert_eq!(text("sqrt(16)").as_deref(), Some("4"));
        assert_eq!(text("2pi").as_deref(), Some("6.28318530718"));
        assert_eq!(text("2(3+4)").as_deref(), Some("14"));
        assert_eq!(text("5!").as_deref(), Some("120"));
        assert_eq!(text("log(1000)").as_deref(), Some("3"));
        assert_eq!(text("log(8, 2)").as_deref(), Some("3"));
        assert_eq!(text("max(1, 7, 3)").as_deref(), Some("7"));
        assert_eq!(text("2e").as_deref(), Some("5.43656365692"));
    }

    #[test]
    fn plain_numbers_and_words_are_not_calculations() {
        for q in ["", "42", "pi", "2048", "firefox", "7zip", "1password", "log", "x-term", "2+", "(1", "1/0", "sqrt(-1)", "171!", "1.2.3"] {
            assert_eq!(evaluate(q), None, "{q:?}");
        }
        assert_eq!(text("= 42").as_deref(), Some("42"), "leading = forces a result");
        assert_eq!(text("=pi").as_deref(), Some("3.14159265359"));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(1e20), "1e20");
        assert_eq!(format_number(1.5e-9), "1.5e-9");
        assert_eq!(format_number(123456.789), "123456.789");
        assert_eq!(format_number(-2.5), "-2.5");
    }
}
