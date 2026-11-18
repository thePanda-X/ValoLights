//! Number formatting and rounding rules used for generated filter graphs, ASS files and JSON caches, so the
//! output is stable and byte-identical to the reference files in the tests. The rules are Python's well-known
//! ones (round half to even, shortest round-trip float repr, `json.dumps` escaping), referred to as such below.

/// Round half to even, as an integer.
pub fn round_i(v: f64) -> i64 {
    vl_engine::layout::round_half_even(v)
}

/// Python's `int(v)`: truncation toward zero.
pub fn trunc_i(v: f64) -> i64 {
    v.trunc() as i64
}

/// Python's floor division of integers.
pub fn floordiv(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Python's `f"{v:.{prec}f}"` (Rust and Python both round the exact binary value half to even).
pub fn f(v: f64, prec: usize) -> String {
    format!("{v:.prec$}")
}

/// Python's `round(v, n)` for floats.
pub fn round_n(v: f64, n: usize) -> f64 {
    if !v.is_finite() {
        return v;
    }
    f(v, n).parse().unwrap_or(v)
}

/// Python's `repr(float)` / `str(float)`.
pub fn repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let e = format!("{v:e}"); // shortest round-trip digits, e.g. "1.5e-7"
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let s = format!("{v}");
        if s.contains('.') {
            s
        } else {
            s + ".0"
        }
    } else {
        format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// A number that may be integral: integral values print without ".0".
pub fn num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        repr(v)
    }
}

/// `json.dumps` of a string (ensure_ascii=True).
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `json.dumps` of a float.
pub fn json_f(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else {
        repr(v)
    }
}

/// `json.dumps` of a list of floats (default separators).
pub fn json_list(v: &[f64]) -> String {
    format!("[{}]", v.iter().map(|x| json_f(*x)).collect::<Vec<_>>().join(", "))
}

/// `subprocess.list2cmdline` (MS C runtime quoting rules).
pub fn list2cmdline<S: AsRef<str>>(args: &[S]) -> String {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        let arg = a.as_ref();
        if i > 0 {
            out.push(' ');
        }
        let need = arg.is_empty() || arg.contains(' ') || arg.contains('\t');
        if need {
            out.push('"');
        }
        let mut bs = String::new();
        for c in arg.chars() {
            if c == '\\' {
                bs.push(c);
            } else if c == '"' {
                out.push_str(&"\\".repeat(bs.len() * 2));
                bs.clear();
                out.push_str("\\\"");
            } else {
                if !bs.is_empty() {
                    out.push_str(&bs);
                    bs.clear();
                }
                out.push(c);
            }
        }
        if !bs.is_empty() {
            out.push_str(&bs);
        }
        if need {
            out.push_str(&bs);
            out.push('"');
        }
    }
    out
}

/// numpy's default (linear) percentile.
pub fn percentile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let pos = (s.len() - 1) as f64 * q / 100.0;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(s.len() - 1);
    let frac = pos - lo as f64;
    // numpy's _lerp: a + (b - a) * t, switched to b - (b - a) * (1 - t) for t >= 0.5
    let (a, b) = (s[lo], s[hi]);
    let d = b - a;
    if frac >= 0.5 {
        b - d * (1.0 - frac)
    } else {
        a + d * frac
    }
}

/// numpy median.
pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Python's `Path.write_text`: text mode, so "\n" becomes the platform line separator ("\r\n" on Windows).
pub fn text_mode(s: &str) -> String {
    if cfg!(windows) {
        s.replace('\n', "\r\n")
    } else {
        s.to_string()
    }
}

pub fn write_text(path: &std::path::Path, s: &str) -> std::io::Result<()> {
    std::fs::write(path, text_mode(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats() {
        assert_eq!(repr(1.0), "1.0");
        assert_eq!(repr(0.1), "0.1");
        assert_eq!(repr(1e-5), "1e-05");
        assert_eq!(repr(1.5e16), "1.5e+16");
        assert_eq!(repr(1763391234.1234567), "1763391234.1234567");
        assert_eq!(f(0.125, 2), "0.12");
        assert_eq!(f(0.375, 2), "0.38");
        assert_eq!(f(2.5, 0), "2");
        assert_eq!(round_n(2.675, 2), 2.67);
        assert_eq!(floordiv(-3, 2), -2);
        assert_eq!(list2cmdline(&["a b", "c", "", "d\\\"e"]), "\"a b\" c \"\" d\\\\\\\"e");
        assert_eq!(json_str("\u{e9}\""), "\"\\u00e9\\\"\"");
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 5.0), 1.15);
    }
}
