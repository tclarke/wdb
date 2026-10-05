#![allow(dead_code)]
use std::fmt;
use std::fs;
use std::path::Path;

/// Sign-magnitude 8-digit decimal number, scaled by 10^7.
/// +0 (digits=0, negative=false) and -0 (digits=0, negative=true) are distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitchNum {
    pub magnitude: u64, // absolute value × 10^7, max 99_999_999
    pub negative: bool,
}

impl WitchNum {
    pub const POS_ZERO: WitchNum = WitchNum { magnitude: 0, negative: false };
    pub const NEG_ZERO: WitchNum = WitchNum { magnitude: 0, negative: true };

    pub fn new(magnitude: u64, negative: bool) -> Self {
        WitchNum { magnitude, negative }
    }

    pub fn from_i64(v: i64) -> Self {
        if v < 0 {
            WitchNum { magnitude: (-v) as u64, negative: true }
        } else {
            WitchNum { magnitude: v as u64, negative: false }
        }
    }

    pub fn to_i64(self) -> i64 {
        if self.negative {
            -(self.magnitude as i64)
        } else {
            self.magnitude as i64
        }
    }

    pub fn is_positive(self) -> bool {
        !self.negative
    }

    pub fn is_negative(self) -> bool {
        self.negative
    }

    pub fn is_zero(self) -> bool {
        self.magnitude == 0
    }

    pub fn is_valid(self) -> bool {
        self.magnitude <= 99_999_999
    }
}

impl fmt::Display for WitchNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.negative { '-' } else { '+' };
        // format as +D.DDDDDDD
        let m = self.magnitude;
        let first = m / 10_000_000;
        let rest = m % 10_000_000;
        write!(f, "{}{}.{:07}", sign, first, rest)
    }
}

/// Sign-magnitude 16-digit decimal accumulator, scaled by 10^7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitchAcc {
    pub magnitude: u64, // max 9_999_999_999_999_999 (fits in u64)
    pub negative: bool,
}

impl WitchAcc {
    pub const POS_ZERO: WitchAcc = WitchAcc { magnitude: 0, negative: false };
    pub const NEG_ZERO: WitchAcc = WitchAcc { magnitude: 0, negative: true };

    pub fn from_witch_num(n: WitchNum) -> Self {
        WitchAcc { magnitude: n.magnitude as u64, negative: n.negative }
    }

    pub fn to_witch_num(self) -> WitchNum {
        WitchNum { magnitude: (self.magnitude % 100_000_000) as u64, negative: self.negative }
    }

    pub fn is_pos_zero(self) -> bool {
        self.magnitude == 0 && !self.negative
    }

    pub fn is_valid(self) -> bool {
        // 16-digit max: 9_999_999_999_999_999
        self.magnitude <= 9_999_999_999_999_999
    }
}

impl fmt::Display for WitchAcc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.negative { '-' } else { '+' };
        let m = self.magnitude;
        let hi = m / 100_000_000;
        let lo = m % 100_000_000;
        let hi_first = hi / 10_000_000;
        let hi_rest = hi % 10_000_000;
        write!(f, "{}{}.{:07}{:08}", sign, hi_first, hi_rest, lo)
    }
}

/// An entry on a tape: block marker, order, or number.
#[derive(Clone, Debug)]
pub enum TapeEntry {
    Block(u8),
    Order(u32),   // 5-digit: OSSDD
    Number(WitchNum),
}

impl fmt::Display for TapeEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TapeEntry::Block(b) => write!(f, "[{}]", b),
            TapeEntry::Order(o) => write!(f, "{:05}", o),
            TapeEntry::Number(n) => write!(f, "{}", n),
        }
    }
}

/// A tape loaded into a reader.
#[derive(Clone, Debug)]
pub struct Tape {
    pub entries: Vec<TapeEntry>,
    pub pos: usize,
    pub looped: bool,
}

impl Tape {
    pub fn new(entries: Vec<TapeEntry>, looped: bool) -> Self {
        Tape { entries, pos: 0, looped }
    }

    /// Advance pos past current entry, wrapping if looped.
    pub fn advance(&mut self) {
        self.pos += 1;
        if self.looped && self.pos >= self.entries.len() {
            self.pos = 0;
        }
    }

    /// Peek at current entry without advancing.
    pub fn current(&self) -> Option<&TapeEntry> {
        self.entries.get(self.pos)
    }

    /// Advance and return the entry that was current (consume it).
    pub fn consume(&mut self) -> Option<TapeEntry> {
        let entry = self.entries.get(self.pos).cloned();
        self.advance();
        entry
    }

    /// Line number of current position (1-based, counting entries).
    pub fn line(&self) -> usize {
        self.pos + 1
    }
}

/// Parse a tape file. Returns a Vec of (tape_number, Tape).
pub fn parse_tape_file(path: &Path) -> Result<Vec<(usize, Tape)>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    parse_tape_content(&content)
}

pub fn parse_tape_content(content: &str) -> Result<Vec<(usize, Tape)>, String> {
    let mut tapes: Vec<(usize, Tape)> = Vec::new();
    let mut current_tape_num: Option<usize> = None;
    let mut current_looped = false;
    let mut current_entries: Vec<TapeEntry> = Vec::new();

    for (lineno, raw_line) in content.lines().enumerate() {
        let lineno = lineno + 1;
        // strip comment
        let line = if let Some(idx) = raw_line.find(';') {
            &raw_line[..idx]
        } else {
            raw_line
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // tape header: tape<n>(looped) or tape<n>(straight)
        if line.starts_with("tape") {
            // save previous tape if any
            if let Some(num) = current_tape_num {
                let entries = std::mem::take(&mut current_entries);
                tapes.push((num, Tape::new(entries, current_looped)));
            }
            let (num, looped) = parse_tape_header(line, lineno)?;
            current_tape_num = Some(num);
            current_looped = looped;
            current_entries = Vec::new();
            continue;
        }

        if current_tape_num.is_none() {
            return Err(format!("line {}: data before tape header", lineno));
        }

        // parse entries from this line (may have multiple whitespace-separated tokens)
        let entries = parse_line_entries(line, lineno)?;
        current_entries.extend(entries);
    }

    if let Some(num) = current_tape_num {
        let entries = std::mem::take(&mut current_entries);
        tapes.push((num, Tape::new(entries, current_looped)));
    }

    Ok(tapes)
}

fn parse_tape_header(line: &str, lineno: usize) -> Result<(usize, bool), String> {
    // tape<n>(looped) or tape<n>(straight)
    let rest = &line["tape".len()..];
    let paren = rest
        .find('(')
        .ok_or_else(|| format!("line {}: bad tape header '{}'", lineno, line))?;
    let num_str = rest[..paren].trim();
    let num: usize = num_str
        .parse()
        .map_err(|_| format!("line {}: bad tape number '{}'", lineno, num_str))?;
    if num < 1 || num > 7 {
        return Err(format!("line {}: tape number {} out of range 1-7", lineno, num));
    }
    let kind = &rest[paren + 1..];
    let kind = kind.trim_end_matches(')').trim();
    let looped = match kind {
        "looped" => true,
        "straight" => false,
        _ => return Err(format!("line {}: expected 'looped' or 'straight', got '{}'", lineno, kind)),
    };
    Ok((num, looped))
}

fn parse_line_entries(line: &str, lineno: usize) -> Result<Vec<TapeEntry>, String> {
    let mut entries = Vec::new();

    // Remove all internal whitespace from the line for parsing
    // (the spec says whitespace inside numbers is ignored)
    // But we need to handle block markers [n] as tokens
    // Strategy: scan character by character
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' => { i += 1; }
            '[' => {
                // block marker [n]
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                let num_str = num_str.trim();
                let b: u8 = num_str
                    .parse()
                    .map_err(|_| format!("line {}: bad block marker '[{}]'", lineno, num_str))?;
                if b > 9 {
                    return Err(format!("line {}: block marker {} out of range 0-9", lineno, b));
                }
                entries.push(TapeEntry::Block(b));
                if i < chars.len() { i += 1; } // skip ']'
            }
            '+' | '-' => {
                // signed number: +DDDDDDDD or -DDDDDDDD (with optional decimal point after first digit)
                let negative = chars[i] == '-';
                i += 1;
                let digits = collect_digits(&chars, &mut i);
                if digits.is_empty() {
                    return Err(format!("line {}: sign without digits", lineno));
                }
                let n = parse_signed_digits(&digits, lineno)?;
                entries.push(TapeEntry::Number(WitchNum::new(n, negative)));
            }
            '*' => {
                // short positive: *DDDDD = +DDDDD000
                i += 1;
                let digits = collect_digits(&chars, &mut i);
                if digits.is_empty() {
                    return Err(format!("line {}: '*' without digits", lineno));
                }
                let n = parse_short_digits(&digits, lineno)?;
                entries.push(TapeEntry::Number(WitchNum::new(n, false)));
            }
            c if c.is_ascii_digit() => {
                // either a 5-digit order or something else
                let digits = collect_digits(&chars, &mut i);
                if digits.len() != 5 {
                    return Err(format!(
                        "line {}: expected 5-digit order, got {} digits: '{}'",
                        lineno,
                        digits.len(),
                        digits.iter().collect::<String>()
                    ));
                }
                let order: u32 = digits.iter().collect::<String>().parse().unwrap();
                entries.push(TapeEntry::Order(order));
            }
            c => {
                return Err(format!("line {}: unexpected character '{}'", lineno, c));
            }
        }
    }

    Ok(entries)
}

/// Collect digit characters (ignoring whitespace and optional decimal point after first digit).
fn collect_digits(chars: &[char], i: &mut usize) -> Vec<char> {
    let mut digits = Vec::new();
    let mut saw_first = false;
    while *i < chars.len() {
        match chars[*i] {
            ' ' | '\t' => { *i += 1; }
            '.' if saw_first && digits.len() == 1 => {
                // decimal point after first digit: ignore it
                *i += 1;
            }
            c if c.is_ascii_digit() => {
                digits.push(c);
                saw_first = true;
                *i += 1;
            }
            _ => break,
        }
    }
    digits
}

/// Parse 8-digit number (possibly with decimal after first) to magnitude × 10^7.
fn parse_signed_digits(digits: &[char], lineno: usize) -> Result<u64, String> {
    if digits.len() != 8 {
        return Err(format!(
            "line {}: signed number must have 8 digits, got {}: '{}'",
            lineno,
            digits.len(),
            digits.iter().collect::<String>()
        ));
    }
    let s: String = digits.iter().collect();
    let n: u64 = s.parse().map_err(|_| format!("line {}: bad digits '{}'", lineno, s))?;
    if n > 99_999_999 {
        return Err(format!("line {}: number {} out of range", lineno, n));
    }
    Ok(n)
}

/// Parse 5-digit short number to magnitude × 10^7 (pad with 3 trailing zeros).
fn parse_short_digits(digits: &[char], lineno: usize) -> Result<u64, String> {
    if digits.len() != 5 {
        return Err(format!(
            "line {}: short number (*) must have 5 digits, got {}: '{}'",
            lineno,
            digits.len(),
            digits.iter().collect::<String>()
        ));
    }
    let s: String = digits.iter().collect();
    let n: u64 = s.parse().map_err(|_| format!("line {}: bad digits '{}'", lineno, s))?;
    Ok(n * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_tape() {
        // *10000 = +1.0000000 (5 digits × 1000 = 10_000_000)
        let content = "tape1(looped)\n[1]\n*10000\n11010\n00100\n";
        let tapes = parse_tape_content(content).unwrap();
        assert_eq!(tapes.len(), 1);
        let (num, tape) = &tapes[0];
        assert_eq!(*num, 1);
        assert!(tape.looped);
        assert_eq!(tape.entries.len(), 4);
        assert!(matches!(tape.entries[0], TapeEntry::Block(1)));
        assert!(matches!(tape.entries[1], TapeEntry::Number(n) if n.magnitude == 10_000_000 && !n.negative));
        assert!(matches!(tape.entries[2], TapeEntry::Order(11010)));
        assert!(matches!(tape.entries[3], TapeEntry::Order(100)));
    }

    #[test]
    fn witch_num_display() {
        let n = WitchNum::new(12_345_678, false);
        assert_eq!(format!("{}", n), "+1.2345678");
        let n = WitchNum::new(0, true);
        assert_eq!(format!("{}", n), "-0.0000000");
    }
}
