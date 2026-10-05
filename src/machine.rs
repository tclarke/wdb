#![allow(dead_code)]
use crate::tape::{Tape, TapeEntry, WitchAcc, WitchNum};
use std::fmt;

/// Why the machine stopped.
#[derive(Clone, Debug, PartialEq)]
pub enum HaltReason {
    Finish,
    Signal,
    Overflow,
    DivideByPosZero,
    ConditionalJumpNoTest,
    TapeExhausted(usize),
    TapeNotLoaded(usize),
    InvalidOrder(u32),
    InvalidAddress(u8),
    NoLayout,
    SameGroupViolation,
}

impl fmt::Display for HaltReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HaltReason::Finish => write!(f, "FINISH"),
            HaltReason::Signal => write!(f, "SIGNAL"),
            HaltReason::Overflow => write!(f, "OVERFLOW: result magnitude >= 10"),
            HaltReason::DivideByPosZero => write!(f, "DIVIDE BY +0"),
            HaltReason::ConditionalJumpNoTest => write!(f, "CONDITIONAL JUMP: no prior sign test"),
            HaltReason::TapeExhausted(n) => write!(f, "TAPE {} EXHAUSTED (end reached)", n),
            HaltReason::TapeNotLoaded(n) => write!(f, "TAPE {} NOT LOADED", n),
            HaltReason::InvalidOrder(o) => write!(f, "INVALID ORDER: {:05}", o),
            HaltReason::InvalidAddress(a) => write!(f, "INVALID ADDRESS: {:02}", a),
            HaltReason::NoLayout => write!(f, "NO LAYOUT SET (output before 07n order)"),
            HaltReason::SameGroupViolation => write!(f, "BOTH ADDRESSES IN SAME GROUP"),
        }
    }
}

/// Instruction pointer.
#[derive(Clone, Copy, Debug)]
pub enum IP {
    /// reader 0-based (tape 1 = index 0)
    Tape { reader: usize, pos: usize },
    Store(usize),
}

/// Output event from an order.
#[derive(Clone, Debug)]
pub enum Output {
    Print(String),
    Perforate(String),
}

pub struct Machine {
    /// stores[0] = address 10 .. stores[89] = address 99
    pub stores: [WitchNum; 90],
    pub acc: WitchAcc,
    pub sign_flag: Option<bool>,
    pub layout: Option<u8>,
    pub shift: Option<u8>, // 1-9 = shift A-J; None = B (default)
    pub tapes: [Option<Tape>; 7],
    pub ip: IP,
    pub halted: bool,
    pub halt_reason: Option<HaltReason>,
    /// column counter for multi-column print layouts
    pub col: usize,
}

impl Machine {
    pub fn new() -> Self {
        Machine {
            stores: [WitchNum::POS_ZERO; 90],
            acc: WitchAcc::POS_ZERO,
            sign_flag: None,
            layout: None,
            shift: None,
            tapes: std::array::from_fn(|_| None),
            ip: IP::Tape { reader: 0, pos: 0 },
            halted: false,
            halt_reason: None,
            col: 0,
        }
    }

    /// Reset state; keep tapes loaded. Run startup sequence.
    pub fn reset(&mut self) {
        self.stores = [WitchNum::POS_ZERO; 90];
        self.acc = WitchAcc::POS_ZERO;
        self.sign_flag = None;
        self.layout = None;
        self.shift = None;
        self.halted = false;
        self.halt_reason = None;
        self.col = 0;
        // startup: find block marker 1 on tape 1
        let pos = if let Some(tape) = &self.tapes[0] {
            tape.entries.iter().position(|e| matches!(e, TapeEntry::Block(1))).unwrap_or(0)
        } else {
            0
        };
        // position after the block marker
        let start_pos = if let Some(tape) = &self.tapes[0] {
            if pos < tape.entries.len() { pos + 1 } else { 0 }
        } else {
            0
        };
        if let Some(tape) = &mut self.tapes[0] {
            tape.pos = start_pos;
        }
        self.ip = IP::Tape { reader: 0, pos: start_pos };
    }

    pub fn load_tape(&mut self, reader: usize, tape: Tape) {
        if (1..=7).contains(&reader) {
            self.tapes[reader - 1] = Some(tape);
        }
    }

    pub fn unload_tape(&mut self, reader: usize) {
        if (1..=7).contains(&reader) {
            self.tapes[reader - 1] = None;
        }
    }

    /// Execute one order from the current IP.
    pub fn step(&mut self) -> Result<Vec<Output>, HaltReason> {
        if self.halted {
            return Err(self.halt_reason.clone().unwrap_or(HaltReason::Finish));
        }
        let order = self.fetch_order()?;
        let result = self.execute_order(order);
        // sync IP.pos from tape.pos — needed when data was consumed from the
        // active execution tape (e.g. input order reads from same tape as instructions)
        if let IP::Tape { reader, ref mut pos } = self.ip {
            if let Some(tape) = &self.tapes[reader] {
                *pos = tape.pos;
            }
        }
        result
    }

    /// Execute a given 5-digit order without changing the IP (for `exec` command).
    pub fn exec_single(&mut self, order: u32) -> Result<Vec<Output>, HaltReason> {
        let saved_ip = self.ip;
        let saved_halted = self.halted;
        let saved_reason = self.halt_reason.clone();
        self.halted = false;
        let result = self.execute_order(order);
        // restore IP (exec shouldn't move the tape)
        self.ip = saved_ip;
        // on error from exec, restore halted state so debugger can report the error
        // without leaving machine in halted state (exec is diagnostic)
        if result.is_err() {
            self.halted = saved_halted;
            self.halt_reason = saved_reason;
        }
        result
    }

    /// Advance IP without executing.
    pub fn skip(&mut self) -> Result<(), HaltReason> {
        match &mut self.ip {
            IP::Tape { reader, pos } => {
                let r = *reader;
                let tape = self.tapes[r].as_mut().ok_or(HaltReason::TapeNotLoaded(r + 1))?;
                tape.pos = *pos;
                tape.advance();
                *pos = tape.pos;
                Ok(())
            }
            IP::Store(addr) => {
                *addr += 1;
                Ok(())
            }
        }
    }

    fn fetch_order(&mut self) -> Result<u32, HaltReason> {
        match self.ip {
            IP::Tape { reader, ref mut pos } => {
                let tape = self.tapes[reader].as_mut().ok_or(HaltReason::TapeNotLoaded(reader + 1))?;
                tape.pos = *pos;
                loop {
                    match tape.entries.get(tape.pos).cloned() {
                        None => {
                            if tape.looped && !tape.entries.is_empty() {
                                tape.pos = 0;
                            } else {
                                return Err(HaltReason::TapeExhausted(reader + 1));
                            }
                        }
                        Some(TapeEntry::Block(_)) | Some(TapeEntry::Number(_)) => {
                            tape.advance();
                        }
                        Some(TapeEntry::Order(o)) => {
                            tape.advance();
                            *pos = tape.pos;
                            return Ok(o);
                        }
                    }
                }
            }
            IP::Store(addr) => {
                if !(10..=99).contains(&addr) {
                    return Err(HaltReason::InvalidAddress(addr as u8));
                }
                let n = self.stores[addr - 10];
                let order = (n.magnitude / 1000) as u32;
                self.ip = IP::Store(addr + 1);
                Ok(order)
            }
        }
    }

    fn execute_order(&mut self, order: u32) -> Result<Vec<Output>, HaltReason> {
        let opcode = order / 10000;
        let src = ((order / 100) % 100) as u8;
        let dst = (order % 100) as u8;

        match opcode {
            1 => self.do_add(src, dst, false, false),
            2 => self.do_add(src, dst, true, false),
            3 => self.do_add(src, dst, false, true),
            4 => self.do_add(src, dst, true, true),
            5 => self.do_multiply(src, dst),
            6 => self.do_divide(src, dst),
            7 => self.do_pos_modulus(src, dst),
            0 => self.do_control(order),
            _ => Err(HaltReason::InvalidOrder(order)),
        }
    }

    // ---- Arithmetic ----

    fn do_add(&mut self, src: u8, dst: u8, clear_src: bool, subtract: bool) -> Result<Vec<Output>, HaltReason> {
        self.check_same_group(src, dst, clear_src)?;

        let src_val = self.read_addr_consuming(src)?;

        let shift = self.shift;
        let shifted = if let Some(s) = shift {
            if s != 2 { // not B = straight-through
                self.shift = None;
                apply_shift(src_val, s, dst == 9)
            } else {
                self.shift = None;
                src_val
            }
        } else {
            src_val
        };

        // For output destinations (printer 01/03, perforator 02/04) and drain (00),
        // there is no "read then add" — just send the value directly.
        let result = if dst <= 7 {
            shifted
        } else {
            let dst_val = self.read_addr_for_add(dst)?;
            decimal_add(dst_val, shifted, subtract)?
        };

        if clear_src && src >= 10 {
            let sign_result = self.stores[(src - 10) as usize].negative;
            self.stores[(src - 10) as usize] = if sign_result { WitchNum::NEG_ZERO } else { WitchNum::POS_ZERO };
        }
        self.write_addr(dst, result)
    }

    fn do_multiply(&mut self, src: u8, dst: u8) -> Result<Vec<Output>, HaltReason> {
        if src <= 9 || dst <= 9 {
            return Err(HaltReason::InvalidOrder(5 * 10000 + (src as u32) * 100 + dst as u32));
        }
        self.check_same_group(src, dst, false)?;

        let multiplicand = self.stores[(src - 10) as usize].to_i64();
        let multiplier = self.stores[(dst - 10) as usize].to_i64();

        // Both at scale 10^-7. Product is at scale 10^-14 = 10^-7 relative to acc scale.
        // acc += multiplicand * multiplier / 10^7
        let prod = multiplicand * multiplier; // 10^-14 scale
        let prod_acc = prod / 10_000_000i64; // 10^-7 scale

        let acc_val = if self.acc.negative {
            -(self.acc.magnitude as i64)
        } else {
            self.acc.magnitude as i64
        };
        let new_acc = acc_val + prod_acc;
        if new_acc.unsigned_abs() > 9_999_999_999_999_999 {
            self.halted = true;
            self.halt_reason = Some(HaltReason::Overflow);
            return Err(HaltReason::Overflow);
        }
        self.acc = WitchAcc { magnitude: new_acc.unsigned_abs(), negative: new_acc < 0 };

        // Clear multiplier; sign of cleared register: negative multiplier -> -0, positive -> +0
        self.stores[(dst - 10) as usize] = if multiplier < 0 { WitchNum::NEG_ZERO } else { WitchNum::POS_ZERO };

        Ok(Vec::new())
    }

    fn do_divide(&mut self, src: u8, dst: u8) -> Result<Vec<Output>, HaltReason> {
        if src <= 9 || dst <= 9 {
            return Err(HaltReason::InvalidOrder(6 * 10000 + (src as u32) * 100 + dst as u32));
        }
        self.check_same_group(src, dst, false)?;

        if self.acc.is_pos_zero() {
            self.halted = true;
            self.halt_reason = Some(HaltReason::DivideByPosZero);
            return Err(HaltReason::DivideByPosZero);
        }

        let divisor = self.stores[(src - 10) as usize].to_i64();
        if divisor == 0 {
            self.halted = true;
            self.halt_reason = Some(HaltReason::DivideByPosZero);
            return Err(HaltReason::DivideByPosZero);
        }

        // acc is dividend (at 10^-7 scale), divisor at 10^-7 scale.
        // quotient_real = acc_real / divisor_real = acc_magnitude / divisor_magnitude (10^7 cancels)
        // quotient at store scale (10^-7) = quotient_real * 10^7 = acc_magnitude * 10^7 / divisor_magnitude
        let acc_signed = if self.acc.negative {
            -(self.acc.magnitude as i64)
        } else {
            self.acc.magnitude as i64
        };

        // Compute with i128 to avoid overflow in intermediate
        let num = (acc_signed as i128) * 10_000_000;
        let div = divisor as i128;
        let quotient = num / div;
        let remainder = acc_signed - ((quotient / 10_000_000) * divisor as i128) as i64;

        let quot_mag = quotient.unsigned_abs();
        if quot_mag >= 100_000_000 {
            self.halted = true;
            self.halt_reason = Some(HaltReason::Overflow);
            return Err(HaltReason::Overflow);
        }

        let dst_val = self.stores[(dst - 10) as usize].to_i64();
        let new_dst = dst_val + quotient as i64;
        if new_dst.unsigned_abs() >= 100_000_000 {
            self.halted = true;
            self.halt_reason = Some(HaltReason::Overflow);
            return Err(HaltReason::Overflow);
        }
        self.stores[(dst - 10) as usize] = WitchNum::from_i64(new_dst);

        let rem_mag = remainder.unsigned_abs();
        self.acc = WitchAcc { magnitude: rem_mag, negative: remainder < 0 };

        Ok(Vec::new())
    }

    fn do_pos_modulus(&mut self, src: u8, dst: u8) -> Result<Vec<Output>, HaltReason> {
        self.check_same_group(src, dst, false)?;
        let src_val = self.read_addr_consuming(src)?;

        let shift = self.shift;
        let shifted = if let Some(s) = shift {
            self.shift = None;
            apply_shift(src_val, s, dst == 9)
        } else {
            src_val
        };

        let dst_val = self.read_addr_for_add(dst)?;
        // always add the absolute value (if src was negative, subtract the negated absolute)
        // i.e., add |src| — which is: if shifted is negative, result = dst - |shifted|? No.
        // "positive modulus": add the absolute value of src to dst.
        // if src positive: add; if src negative: still add (the absolute value).
        // Wait, re-reading: "If it is positive it behaves as add-and-hold; if negative it behaves as subtract-and-hold"
        // That means: add |src| either way? No — "subtract negative" means dst -= |negative_src|?
        // Actually re-reading again: "positive modulus" = transfer the absolute value.
        // "if positive: add-and-hold" => dst += src
        // "if negative: subtract-and-hold" => dst -= src (which is dst += |src| since src is negative)
        // So in both cases dst += |src| = dst += abs(src). Yes, always add the magnitude.
        let abs_shifted = WitchNum::new(shifted.magnitude, false);
        // But the subtract flag is false (always add the magnitude)
        let result = decimal_add(dst_val, abs_shifted, false)?;
        self.write_addr(dst, result)
    }

    fn do_control(&mut self, order: u32) -> Result<Vec<Output>, HaltReason> {
        let second = (order / 1000) % 10;
        let third = (order / 100) % 10;
        let dst = (order % 100) as u8;

        match order {
            0 => Ok(Vec::new()),
            100 => {
                self.halted = true;
                self.halt_reason = Some(HaltReason::Finish);
                Err(HaltReason::Finish)
            }
            200 => {
                self.halted = true;
                self.halt_reason = Some(HaltReason::Signal);
                Err(HaltReason::Signal)
            }
            _ if second == 1 && third == 1 => {
                let val = self.read_store_or_acc(dst)?;
                self.sign_flag = Some(val.is_positive());
                Ok(Vec::new())
            }
            _ if second == 1 && third == 2 => {
                let val = self.read_store_or_acc(dst)?;
                self.sign_flag = Some(val.is_negative());
                Ok(Vec::new())
            }
            _ if second == 2 && third == 1 => {
                self.do_transfer(dst)?;
                Ok(Vec::new())
            }
            _ if second == 2 && third == 2 => {
                match self.sign_flag {
                    None => {
                        self.halted = true;
                        self.halt_reason = Some(HaltReason::ConditionalJumpNoTest);
                        Err(HaltReason::ConditionalJumpNoTest)
                    }
                    Some(false) => Ok(Vec::new()),
                    Some(true) => {
                        self.do_transfer(dst)?;
                        Ok(Vec::new())
                    }
                }
            }
            _ if second == 3 => {
                let block = third as u8;
                self.do_search(block, dst)?;
                Ok(Vec::new())
            }
            _ if second == 5 => {
                let block = third as u8;
                match self.sign_flag {
                    Some(true) => {
                        self.do_search(block, dst)?;
                    }
                    _ => {}
                }
                Ok(Vec::new())
            }
            _ if second == 7 => {
                self.layout = Some(third as u8);
                Ok(Vec::new())
            }
            _ if second == 8 => {
                self.shift = Some(third as u8);
                Ok(Vec::new())
            }
            _ => Err(HaltReason::InvalidOrder(order)),
        }
    }

    fn do_transfer(&mut self, rr: u8) -> Result<(), HaltReason> {
        if (1..=7).contains(&rr) {
            let reader = (rr - 1) as usize;
            let pos = self.tapes[reader].as_ref().map(|t| t.pos).unwrap_or(0);
            self.ip = IP::Tape { reader, pos };
        } else if rr >= 10 {
            self.ip = IP::Store(rr as usize);
        } else {
            return Err(HaltReason::InvalidAddress(rr));
        }
        Ok(())
    }

    fn do_search(&mut self, block: u8, reader: u8) -> Result<(), HaltReason> {
        if !(1..=7).contains(&reader) {
            return Err(HaltReason::InvalidAddress(reader));
        }
        let r = (reader - 1) as usize;
        let tape = self.tapes[r].as_mut().ok_or(HaltReason::TapeNotLoaded(reader as usize))?;

        let len = tape.entries.len();
        if len == 0 {
            return Err(HaltReason::TapeExhausted(reader as usize));
        }

        let start = tape.pos;
        let max_iters = if tape.looped { len + 1 } else { len };
        let mut found = false;

        for step in 0..max_iters {
            let idx = if tape.looped { (start + step) % len } else { start + step };
            if idx >= len {
                break;
            }
            if matches!(tape.entries[idx], TapeEntry::Block(b) if b == block) {
                let new_pos = if tape.looped { (idx + 1) % len } else { (idx + 1).min(len) };
                tape.pos = new_pos;
                found = true;
                break;
            }
        }

        if !found {
            return Err(HaltReason::TapeExhausted(reader as usize));
        }

        if let IP::Tape { reader: cur_r, pos } = &mut self.ip {
            if *cur_r == r {
                *pos = self.tapes[r].as_ref().map(|t| t.pos).unwrap_or(0);
            }
        }

        Ok(())
    }

    // ---- Address reads ----

    /// Read a value from an address. Does NOT consume from tape readers (use read_addr_consuming for that).
    pub fn read_addr(&self, addr: u8) -> Result<WitchNum, HaltReason> {
        match addr {
            0 => Ok(random_roundoff()),
            8 => {
                let low7 = (self.acc.magnitude % 10_000_000) as u64;
                Ok(WitchNum::new(low7, self.acc.negative))
            }
            9 => {
                let high8 = (self.acc.magnitude / 100_000_000) as u64;
                Ok(WitchNum::new(high8, self.acc.negative))
            }
            10..=99 => Ok(self.stores[(addr - 10) as usize]),
            _ => Err(HaltReason::InvalidAddress(addr)),
        }
    }

    /// Read, consuming from tape reader if addr is 1-7.
    fn read_addr_consuming(&mut self, addr: u8) -> Result<WitchNum, HaltReason> {
        if (1..=7).contains(&addr) {
            let r = (addr - 1) as usize;
            let tape = self.tapes[r].as_mut().ok_or(HaltReason::TapeNotLoaded(addr as usize))?;
            loop {
                match tape.entries.get(tape.pos).cloned() {
                    None => {
                        if tape.looped && !tape.entries.is_empty() {
                            tape.pos = 0;
                        } else {
                            return Err(HaltReason::TapeExhausted(addr as usize));
                        }
                    }
                    Some(TapeEntry::Number(n)) => {
                        tape.advance();
                        return Ok(n);
                    }
                    Some(_) => {
                        tape.advance();
                    }
                }
            }
        } else {
            self.read_addr(addr)
        }
    }

    /// Read destination value for add/subtract (handles acc specially).
    fn read_addr_for_add(&self, addr: u8) -> Result<WitchNum, HaltReason> {
        match addr {
            9 => {
                // Add into whole acc: read high 8 digits of acc
                let high8 = (self.acc.magnitude / 100_000_000) as u64;
                Ok(WitchNum::new(high8, self.acc.negative))
            }
            _ => self.read_addr(addr),
        }
    }

    /// Read for sign test — store or accumulator (08/09 valid, 00-07 not valid for sign test).
    fn read_store_or_acc(&self, addr: u8) -> Result<WitchNum, HaltReason> {
        match addr {
            8 => {
                let low7 = (self.acc.magnitude % 10_000_000) as u64;
                Ok(WitchNum::new(low7, self.acc.negative))
            }
            9 => {
                let high8 = (self.acc.magnitude / 100_000_000) as u64;
                Ok(WitchNum::new(high8, self.acc.negative))
            }
            10..=99 => Ok(self.stores[(addr - 10) as usize]),
            _ => Err(HaltReason::InvalidAddress(addr)),
        }
    }

    fn write_addr(&mut self, addr: u8, val: WitchNum) -> Result<Vec<Output>, HaltReason> {
        match addr {
            0 => Ok(Vec::new()),
            1 | 3 => {
                let s = self.format_output(val)?;
                Ok(vec![Output::Print(s)])
            }
            2 | 4 => {
                let s = self.format_output(val)?;
                Ok(vec![Output::Perforate(s)])
            }
            5..=7 => Ok(Vec::new()),
            8 => {
                let low7 = val.magnitude % 10_000_000;
                let hi = self.acc.magnitude / 10_000_000;
                self.acc.magnitude = hi * 10_000_000 + low7;
                self.acc.negative = val.negative;
                Ok(Vec::new())
            }
            9 => {
                // Write whole acc: val goes into high 8 digits, low 8 cleared
                self.acc.magnitude = val.magnitude as u64 * 100_000_000;
                self.acc.negative = val.negative;
                Ok(Vec::new())
            }
            10..=99 => {
                self.stores[(addr - 10) as usize] = val;
                Ok(Vec::new())
            }
            _ => Err(HaltReason::InvalidAddress(addr)),
        }
    }

    fn check_same_group(&self, src: u8, dst: u8, clear: bool) -> Result<(), HaltReason> {
        let sg = src / 10;
        let dg = dst / 10;
        if sg == 0 && dg == 0 {
            if !is_permitted_00_09_pair(src, dst, clear) {
                return Err(HaltReason::SameGroupViolation);
            }
        } else if sg == dg {
            return Err(HaltReason::SameGroupViolation);
        }
        Ok(())
    }

    fn format_output(&mut self, val: WitchNum) -> Result<String, HaltReason> {
        let layout = self.layout.ok_or(HaltReason::NoLayout)?;
        let s = format_for_layout(val, layout, &mut self.col);
        Ok(s)
    }

    // ---- Query helpers for debugger ----

    pub fn active_tape_num(&self) -> Option<usize> {
        match self.ip {
            IP::Tape { reader, .. } => Some(reader + 1),
            IP::Store(_) => None,
        }
    }

    pub fn tape_ref(&self, reader: usize) -> Option<&Tape> {
        self.tapes.get(reader - 1)?.as_ref()
    }

    pub fn current_tape_pos(&self) -> Option<usize> {
        match self.ip {
            IP::Tape { pos, .. } => Some(pos),
            _ => None,
        }
    }

    pub fn peek_tape_entries(&self, tape_num: usize, count: usize) -> Vec<(usize, &TapeEntry)> {
        let tape = match self.tapes.get(tape_num - 1) {
            Some(Some(t)) => t,
            _ => return Vec::new(),
        };
        let start = if Some(tape_num) == self.active_tape_num() {
            match self.ip {
                IP::Tape { pos, .. } => pos,
                _ => tape.pos,
            }
        } else {
            tape.pos
        };
        tape.entries.iter().enumerate().skip(start).take(count).collect()
    }
}

fn decimal_add(dst: WitchNum, src: WitchNum, subtract: bool) -> Result<WitchNum, HaltReason> {
    let d = dst.to_i64();
    let s = if subtract { -src.to_i64() } else { src.to_i64() };
    let result = d + s;
    if result.unsigned_abs() >= 100_000_000 {
        return Err(HaltReason::Overflow);
    }
    Ok(WitchNum::from_i64(result))
}

fn apply_shift(val: WitchNum, shift: u8, to_acc: bool) -> WitchNum {
    match shift {
        1 => {
            // Shift A: × 10, discard leftmost digit
            let new_mag = (val.magnitude * 10) % 100_000_000;
            WitchNum::new(new_mag, val.negative)
        }
        2 => val,
        3..=9 => {
            let factor = 10u64.pow((shift - 2) as u32);
            let new_mag = if to_acc {
                // Low digits land in acc — simplified: same as truncation for now
                val.magnitude / factor
            } else {
                val.magnitude / factor
            };
            WitchNum::new(new_mag, val.negative)
        }
        _ => val,
    }
}

fn is_permitted_00_09_pair(src: u8, dst: u8, clear: bool) -> bool {
    if clear {
        return false;
    }
    matches!(
        (src, dst),
        (0, 9) | (1..=7, 0) | (1..=7, 9) | (8, 0) | (8, 1..=4) | (9, 0) | (9, 1..=4)
    )
}

fn random_roundoff() -> WitchNum {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let bit = (t.subsec_nanos() & 1) as u64;
    WitchNum::new(bit, false)
}

fn format_for_layout(val: WitchNum, layout: u8, col: &mut usize) -> String {
    let sign = if val.negative { '-' } else { '+' };
    let m = val.magnitude;
    let first = m / 10_000_000;
    let rest = m % 10_000_000;

    match layout {
        0 => {
            *col = 0;
            "\n\n\n\n\n".to_string()
        }
        1 => format!("{}{}{:07}", sign, first, rest),
        2 => format!("*{:05}", m / 1_000),
        3 => {
            // 8 digits, 5 columns per line, first or intermediate
            let s = format!("{}{}.{:07}", sign, first, rest);
            *col += 1;
            s
        }
        4 => {
            // 8 digits, last on line
            let s = format!("{}{}.{:07}\n", sign, first, rest);
            *col = 0;
            s
        }
        5 => {
            let s = format!("{}{}.{:07}\n\n", sign, first, rest);
            *col = 0;
            s
        }
        6 | 7 => {
            // 6 digits
            let six = m / 100;
            let f6 = six / 100_000;
            let r5 = six % 100_000;
            let s = format!("{}{}.{:05}", sign, f6, r5);
            *col += 1;
            s
        }
        8 => {
            let six = m / 100;
            let f6 = six / 100_000;
            let r5 = six % 100_000;
            let s = format!("{}{}.{:05}\n", sign, f6, r5);
            *col = 0;
            s
        }
        9 => {
            let six = m / 100;
            let f6 = six / 100_000;
            let r5 = six % 100_000;
            let s = format!("{}{}.{:05}\n\n", sign, f6, r5);
            *col = 0;
            s
        }
        _ => format!("{}", val),
    }
}
