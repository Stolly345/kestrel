//! The quick-search window's calculator: `2^10 * 3`, `sqrt(2)/2`, `15% * 80`, `(1+2)*3`.
//! Knows + - * / ^ (or **), %, parentheses, unary minus, pi, e, and the functions sqrt cbrt
//! abs ln log log2 exp sin cos tan asin acos atan round floor ceil (radians).

/// The value of `s` if it is a sum worth showing: it has an operator or a function, not just
/// a number or a word, so typing `2026` or `pi` searches instead.
pub fn eval(s: &str) -> Option<f64> {
    let s = s.trim().trim_start_matches('=').trim();
    let number = s.trim_start_matches('-').chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',');
    if s.is_empty() || number || !s.chars().any(|c| "+-*/^%(×÷".contains(c)) {
        return None;
    }
    let toks = lex(s)?;
    let mut p = Parser { toks, pos: 0 };
    let v = p.expr()?;
    (p.pos == p.toks.len() && v.is_finite()).then_some(v)
}

/// Up to 12 significant digits, no trailing zeros, thousands not grouped (it gets copied).
pub fn format(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.*}", (11 - v.abs().log10().floor() as i32).clamp(0, 15) as usize, v);
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    if s == "0" || s == "-0" { format!("{v:e}") } else { s }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Id(String),
    Op(char),
}

fn lex(s: &str) -> Option<Vec<Tok>> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() || ch == ',' && i > 0 && c.get(i + 1).is_some_and(|d| d.is_ascii_digit()) && c[i - 1].is_ascii_digit() {
            i += 1; // 1,000 is a thousands separator
        } else if ch.is_ascii_digit() || ch == '.' {
            let start = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.' || c[i] == ',' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
                i += 1;
            }
            let n: String = c[start..i].iter().filter(|&&d| d != ',').collect();
            out.push(Tok::Num(n.parse().ok()?));
        } else if ch.is_alphabetic() {
            let start = i;
            while i < c.len() && c[i].is_alphanumeric() {
                i += 1;
            }
            out.push(Tok::Id(c[start..i].iter().collect::<String>().to_lowercase()));
        } else if ch == '*' && c.get(i + 1) == Some(&'*') {
            out.push(Tok::Op('^'));
            i += 2;
        } else if "+-*/^%()×÷".contains(ch) {
            out.push(Tok::Op(match ch {
                '×' => '*',
                '÷' => '/',
                c => c,
            }));
            i += 1;
        } else {
            return None;
        }
    }
    Some(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Tok::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat('+') {
                v += self.term()?;
            } else if self.eat('-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat('*') {
                v *= self.unary()?;
            } else if self.eat('/') {
                v /= self.unary()?;
            } else if matches!(self.peek(), Some(Tok::Op('(') | Tok::Id(_))) {
                v *= self.unary()?; // 2pi, 3(4+1)
            } else {
                return Some(v);
            }
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat('-') {
            return Some(-self.unary()?);
        }
        if self.eat('+') {
            return self.unary();
        }
        self.power()
    }

    /// Right-associative, binds tighter than unary minus on its left: -2^2 = -4.
    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if self.eat('^') {
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    /// `50%` = 0.5
    fn postfix(&mut self) -> Option<f64> {
        let mut v = self.atom()?;
        while self.eat('%') {
            v /= 100.0;
        }
        Some(v)
    }

    fn atom(&mut self) -> Option<f64> {
        match self.toks.get(self.pos).cloned()? {
            Tok::Num(n) => {
                self.pos += 1;
                Some(n)
            }
            Tok::Op('(') => {
                self.pos += 1;
                let v = self.expr()?;
                self.eat(')'); // a missing ) at the end is fine while typing
                Some(v)
            }
            Tok::Id(id) => {
                self.pos += 1;
                match id.as_str() {
                    "pi" | "π" => return Some(std::f64::consts::PI),
                    "e" => return Some(std::f64::consts::E),
                    "tau" => return Some(std::f64::consts::TAU),
                    _ => {}
                }
                let f: fn(f64) -> f64 = match id.as_str() {
                    "sqrt" => f64::sqrt,
                    "cbrt" => f64::cbrt,
                    "abs" => f64::abs,
                    "ln" => f64::ln,
                    "log" | "log10" => f64::log10,
                    "log2" => f64::log2,
                    "exp" => f64::exp,
                    "sin" => f64::sin,
                    "cos" => f64::cos,
                    "tan" => f64::tan,
                    "asin" => f64::asin,
                    "acos" => f64::acos,
                    "atan" => f64::atan,
                    "round" => f64::round,
                    "floor" => f64::floor,
                    "ceil" => f64::ceil,
                    _ => return None,
                };
                Some(f(self.power()?))
            }
            Tok::Op(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums() {
        let e = |s: &str| eval(s).map(format);
        assert_eq!(e("2^10 * 3").as_deref(), Some("3072"));
        assert_eq!(e("2**3").as_deref(), Some("8"));
        assert_eq!(e("1 + 2 * 3").as_deref(), Some("7"));
        assert_eq!(e("(1 + 2) * 3").as_deref(), Some("9"));
        assert_eq!(e("-2^2").as_deref(), Some("-4"));
        assert_eq!(e("2^-1").as_deref(), Some("0.5"));
        assert_eq!(e("10 / 4").as_deref(), Some("2.5"));
        assert_eq!(e("1/3").as_deref(), Some("0.333333333333"));
        assert_eq!(e("15% * 80").as_deref(), Some("12"));
        assert_eq!(e("sqrt(16) + 1").as_deref(), Some("5"));
        assert_eq!(e("2pi + 0").map(|s| s[..6].to_string()).as_deref(), Some("6.2831"));
        assert_eq!(e("1,000 * 3").as_deref(), Some("3000"));
        assert_eq!(e("= 6 × 7").as_deref(), Some("42"));
        assert_eq!(e("(2+3").as_deref(), Some("5"), "an unclosed bracket while typing");
        // searches, not sums
        for s in ["2026", "report", "pi", "-5", "invoice 2026", "a-b", "1/0", "2 +", "my-file.txt", "", "sqrt"] {
            assert_eq!(e(s), None, "{s:?}");
        }
    }
}
