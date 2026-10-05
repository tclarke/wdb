use crate::disasm::disassemble;
use crate::machine::{Machine, Output, IP};
use crate::tape::{parse_tape_file, TapeEntry};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum BpKind {
    Block(u8),
    Line(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Location {
    Store(usize),
    Acc,
    Signal,
    Alarm,
    Sign,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CondOp {
    Lt,
    Gt,
    Eq,
    Le,
    Ge,
    Ne,
    Changes,
}

#[derive(Clone, Debug)]
pub struct Condition {
    pub location: Location,
    pub op: CondOp,
    pub value: Option<i64>,
    pub prev: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Breakpoint {
    pub id: usize,
    pub kind: BpKind,
    pub tape: usize,
    pub enabled: bool,
    pub conditions: Vec<Condition>,
}

pub struct Debugger {
    pub machine: Machine,
    breakpoints: Vec<Breakpoint>,
    next_bp_id: usize,
}

impl Debugger {
    pub fn new() -> Self {
        Debugger {
            machine: Machine::new(),
            breakpoints: Vec::new(),
            next_bp_id: 1,
        }
    }

    /// Execute a debugger command line. Returns lines to print.
    pub fn execute(&mut self, line: &str) -> Vec<String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return Vec::new();
        }

        match parts[0] {
            "run" | "r" => self.cmd_run(),
            "step" | "s" => self.cmd_step(),
            "skip" => self.cmd_skip(),
            "list" | "l" => {
                let tape_num = parts.get(1).and_then(|s| s.parse().ok());
                self.cmd_list(tape_num)
            }
            "print" | "p" => {
                if parts.len() < 2 {
                    return vec!["usage: print <store|acc|signal|alarm|sign|layout|shift>".to_string()];
                }
                self.cmd_print(parts[1])
            }
            "dis" | "d" => {
                let n = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                self.cmd_dis(n)
            }
            "load" => {
                if parts.len() < 2 {
                    return vec!["usage: load <filename> [tapenumber]".to_string()];
                }
                let tape_num = parts.get(2).and_then(|s| s.parse().ok());
                self.cmd_load(parts[1], tape_num)
            }
            "reset" => self.cmd_reset(),
            "clear" => {
                let tape_num = parts.get(1).and_then(|s| s.parse().ok());
                self.cmd_clear(tape_num)
            }
            "exec" => {
                if parts.len() < 2 {
                    return vec!["usage: exec <order>".to_string()];
                }
                self.cmd_exec(parts[1])
            }
            "transfer" => {
                if parts.len() < 2 {
                    return vec!["usage: transfer <tapenumber>".to_string()];
                }
                self.cmd_transfer(parts[1])
            }
            "search" => {
                if parts.len() < 2 {
                    return vec!["usage: search <block> [tapenumber]".to_string()];
                }
                let tape_num = parts.get(2).and_then(|s| s.parse().ok());
                self.cmd_search(parts[1], tape_num)
            }
            "break" | "b" => self.cmd_break(&parts[1..]),
            "dump" => {
                let show_tapes = parts.get(1).map(|&s| s == "tapes").unwrap_or(false);
                let show_dis = parts.get(2).map(|&s| s == "dis").unwrap_or(false);
                self.cmd_dump(show_tapes, show_dis)
            }
            "help" | "h" | "?" => Self::cmd_help(),
            "quit" | "exit" | "q" => std::process::exit(0),
            cmd => vec![format!("unknown command '{}' (type 'help' for help)", cmd)],
        }
    }

    // ---- Commands ----

    fn cmd_run(&mut self) -> Vec<String> {
        if self.machine.halted {
            return vec!["machine halted; use 'reset' to restart".to_string()];
        }
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let _ = ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        });

        let mut output = Vec::new();
        loop {
            if !running.load(Ordering::SeqCst) {
                output.push("^C — interrupted".to_string());
                break;
            }
            if let Some(msg) = self.check_breakpoints() {
                output.push(msg);
                break;
            }
            match self.machine.step() {
                Ok(outs) => output.extend(outs.into_iter().flat_map(format_output)),
                Err(reason) => {
                    output.push(format!("halted: {}", reason));
                    break;
                }
            }
        }
        output
    }

    fn cmd_step(&mut self) -> Vec<String> {
        if self.machine.halted {
            return vec!["machine halted; use 'reset' to restart".to_string()];
        }
        let mut out = Vec::new();
        match self.machine.step() {
            Ok(outs) => out.extend(outs.into_iter().flat_map(format_output)),
            Err(reason) => {
                out.push(format!("halted: {}", reason));
                return out;
            }
        }
        out.extend(self.show_position());
        out
    }

    fn cmd_skip(&mut self) -> Vec<String> {
        match self.machine.skip() {
            Ok(()) => self.show_position(),
            Err(e) => vec![format!("error: {}", e)],
        }
    }

    fn cmd_list(&self, tape_num: Option<usize>) -> Vec<String> {
        let tape_num = tape_num.unwrap_or_else(|| self.machine.active_tape_num().unwrap_or(1));
        let entries = self.machine.peek_tape_entries(tape_num, 20);
        if entries.is_empty() {
            return vec![format!("tape {} not loaded or empty", tape_num)];
        }
        let cur_pos = self.machine.current_tape_pos();
        entries
            .iter()
            .map(|(idx, entry)| {
                let marker = if cur_pos == Some(*idx) { ">" } else { " " };
                format!("{} {:4}: {}", marker, idx + 1, entry)
            })
            .collect()
    }

    fn cmd_print(&self, loc: &str) -> Vec<String> {
        match loc {
            "acc" => vec![format!("acc = {}", self.machine.acc)],
            "signal" | "sig" => vec!["signal = off (simulator)".to_string()],
            "alarm" => vec!["alarm = off (simulator)".to_string()],
            "sign" => vec![format!("sign = {}", match self.machine.sign_flag {
                None => "(not set)".to_string(),
                Some(true) => "positive (true)".to_string(),
                Some(false) => "negative (false)".to_string(),
            })],
            "layout" => vec![format!("layout = {}", self.machine.layout.map(|n| n.to_string()).unwrap_or_else(|| "(none)".to_string()))],
            "shift" => vec![format!("shift = {}", match self.machine.shift {
                None => "B (default, no pending shift)".to_string(),
                Some(n) => {
                    let letters = ['?', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J'];
                    format!("{} (pending)", letters.get(n as usize).unwrap_or(&'?'))
                }
            })],
            s => {
                if let Ok(addr) = s.parse::<u8>()
                    && (10..=99).contains(&addr) {
                        let val = self.machine.stores[(addr - 10) as usize];
                        return vec![format!("store {} = {}", addr, val)];
                    }
                vec![format!("unknown: '{}' (use store 10-99, acc, sign, layout, shift)", s)]
            }
        }
    }

    fn cmd_dis(&self, n: usize) -> Vec<String> {
        let tape_num = self.machine.active_tape_num().unwrap_or(1);
        let entries = self.machine.peek_tape_entries(tape_num, n * 4 + 10);
        let mut result = Vec::new();
        let mut count = 0;
        for (idx, entry) in &entries {
            if count >= n {
                break;
            }
            if let TapeEntry::Order(o) = entry {
                result.push(format!("{:4}: {:05}  {}", idx + 1, o, disassemble(*o)));
                count += 1;
            }
        }
        if result.is_empty() {
            result.push("no orders at current position".to_string());
        }
        result
    }

    fn cmd_load(&mut self, filename: &str, only_tape: Option<usize>) -> Vec<String> {
        match parse_tape_file(Path::new(filename)) {
            Ok(tapes) => {
                let mut loaded = Vec::new();
                for (num, tape) in tapes {
                    if let Some(only) = only_tape
                        && num != only {
                            continue;
                        }
                    let entry_count = tape.entries.len();
                    self.machine.load_tape(num, tape);
                    loaded.push(format!("loaded tape {} ({} entries) from {}", num, entry_count, filename));
                }
                if loaded.is_empty() {
                    vec![format!("no matching tapes in {}", filename)]
                } else {
                    loaded
                }
            }
            Err(e) => vec![format!("error: {}", e)],
        }
    }

    fn cmd_reset(&mut self) -> Vec<String> {
        self.machine.reset();
        vec!["machine reset".to_string()]
    }

    fn cmd_clear(&mut self, tape_num: Option<usize>) -> Vec<String> {
        match tape_num {
            Some(n) if (1..=7).contains(&n) => {
                self.machine.unload_tape(n);
                vec![format!("tape {} unloaded", n)]
            }
            None => {
                for i in 1..=7 {
                    self.machine.unload_tape(i);
                }
                vec!["all tapes unloaded".to_string()]
            }
            Some(n) => vec![format!("invalid tape number {} (must be 1-7)", n)],
        }
    }

    fn cmd_exec(&mut self, order_str: &str) -> Vec<String> {
        let order: u32 = match order_str.trim().parse() {
            Ok(o) if o <= 99999 => o,
            _ => return vec![format!("invalid order '{}' (must be 5-digit number 00000-99999)", order_str)],
        };
        match self.machine.exec_single(order) {
            Ok(outs) => outs.into_iter().flat_map(format_output).collect(),
            Err(e) => vec![format!("error: {}", e)],
        }
    }

    fn cmd_transfer(&mut self, tape_str: &str) -> Vec<String> {
        let n: u8 = match tape_str.parse() {
            Ok(n) if (1..=7).contains(&n) => n,
            _ => return vec![format!("invalid tape number '{}' (must be 1-7)", tape_str)],
        };
        let order = 2 * 10000 + 1000 + n as u32; // 021rr
        match self.machine.exec_single(order) {
            Ok(_) => {
                vec![format!("transferred to tape {}", n)]
            }
            Err(e) => vec![format!("error: {}", e)],
        }
    }

    fn cmd_search(&mut self, block_str: &str, tape_num: Option<usize>) -> Vec<String> {
        let block: u8 = match block_str.parse::<u8>() {
            Ok(b) if b <= 9 => b,
            _ => return vec![format!("invalid block '{}' (must be 0-9)", block_str)],
        };
        let tape_num = tape_num.unwrap_or_else(|| self.machine.active_tape_num().unwrap_or(1));
        let order = 3 * 10000 + (block as u32) * 100 + tape_num as u32;
        match self.machine.exec_single(order) {
            Ok(_) => vec![format!("tape {} positioned at block {}", tape_num, block)],
            Err(e) => vec![format!("error: {}", e)],
        }
    }

    fn cmd_break(&mut self, args: &[&str]) -> Vec<String> {
        if args.is_empty() {
            return self.list_breakpoints();
        }
        match args[0] {
            "block" => {
                if args.len() < 2 {
                    return vec!["usage: break block <block> [tape]".to_string()];
                }
                let block: u8 = match args[1].parse::<u8>() {
                    Ok(b) if b <= 9 => b,
                    _ => return vec![format!("invalid block '{}'", args[1])],
                };
                let tape = args.get(2).and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| self.machine.active_tape_num().unwrap_or(1));
                let id = self.next_bp_id;
                self.next_bp_id += 1;
                self.breakpoints.push(Breakpoint { id, kind: BpKind::Block(block), tape, enabled: true, conditions: Vec::new() });
                vec![format!("breakpoint {} set: block {} on tape {}", id, block, tape)]
            }
            "line" => {
                if args.len() < 2 {
                    return vec!["usage: break line <lineno> [tape]".to_string()];
                }
                let lineno: usize = match args[1].parse() {
                    Ok(n) => n,
                    Err(_) => return vec![format!("invalid line number '{}'", args[1])],
                };
                let tape = args.get(2).and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| self.machine.active_tape_num().unwrap_or(1));
                let id = self.next_bp_id;
                self.next_bp_id += 1;
                self.breakpoints.push(Breakpoint { id, kind: BpKind::Line(lineno), tape, enabled: true, conditions: Vec::new() });
                vec![format!("breakpoint {} set: line {} on tape {}", id, lineno, tape)]
            }
            "dis" => {
                if args.len() < 2 { return vec!["usage: break dis <id>".to_string()]; }
                self.bp_set_enabled(args[1], false)
            }
            "en" => {
                if args.len() < 2 { return vec!["usage: break en <id>".to_string()]; }
                self.bp_set_enabled(args[1], true)
            }
            "rm" => {
                if args.len() < 2 { return vec!["usage: break rm <id>".to_string()]; }
                match args[1].parse::<usize>() {
                    Ok(id) => {
                        let before = self.breakpoints.len();
                        self.breakpoints.retain(|b| b.id != id);
                        if self.breakpoints.len() < before {
                            vec![format!("breakpoint {} removed", id)]
                        } else {
                            vec![format!("breakpoint {} not found", id)]
                        }
                    }
                    Err(_) => vec![format!("invalid id '{}'", args[1])],
                }
            }
            "when" => {
                if args.len() < 3 {
                    return vec!["usage: break when <id> <location> [<op> <value>]".to_string()];
                }
                self.cmd_break_when(args)
            }
            s => vec![format!("unknown break subcommand '{}' (use block, line, dis, en, rm, when)", s)],
        }
    }

    fn bp_set_enabled(&mut self, id_str: &str, enabled: bool) -> Vec<String> {
        match id_str.parse::<usize>() {
            Ok(id) => {
                if let Some(bp) = self.breakpoints.iter_mut().find(|b| b.id == id) {
                    bp.enabled = enabled;
                    vec![format!("breakpoint {} {}", id, if enabled { "enabled" } else { "disabled" })]
                } else {
                    vec![format!("breakpoint {} not found", id)]
                }
            }
            Err(_) => vec![format!("invalid id '{}'", id_str)],
        }
    }

    fn cmd_break_when(&mut self, args: &[&str]) -> Vec<String> {
        // args[0] = "when", args[1] = id, args[2] = location, args[3] = op, args[4] = value
        let id: usize = match args[1].parse() {
            Ok(n) => n,
            Err(_) => return vec![format!("invalid id '{}'", args[1])],
        };
        let location = match parse_location(args[2]) {
            Ok(l) => l,
            Err(e) => return vec![e],
        };
        let (op, value) = if args.len() >= 4 {
            let op = match args[3] {
                "<" => CondOp::Lt,
                ">" => CondOp::Gt,
                "=" | "==" => CondOp::Eq,
                "<=" => CondOp::Le,
                ">=" => CondOp::Ge,
                "!=" => CondOp::Ne,
                "changes" => CondOp::Changes,
                s => return vec![format!("invalid op '{}' (< > = <= >= != changes)", s)],
            };
            let val = if op != CondOp::Changes {
                if args.len() < 5 {
                    return vec!["usage: break when <id> <loc> <op> <value>".to_string()];
                }
                let v: f64 = match args[4].parse() {
                    Ok(v) => v,
                    Err(_) => return vec![format!("invalid value '{}'", args[4])],
                };
                Some((v * 10_000_000.0) as i64)
            } else {
                None
            };
            (op, val)
        } else {
            (CondOp::Eq, Some(1))
        };

        if let Some(bp) = self.breakpoints.iter_mut().find(|b| b.id == id) {
            bp.conditions.push(Condition { location, op, value, prev: None });
            vec![format!("condition added to breakpoint {}", id)]
        } else {
            vec![format!("breakpoint {} not found", id)]
        }
    }

    fn list_breakpoints(&self) -> Vec<String> {
        if self.breakpoints.is_empty() {
            return vec!["no breakpoints".to_string()];
        }
        self.breakpoints.iter().map(|bp| {
            let status = if bp.enabled { "enabled" } else { "disabled" };
            let kind = match bp.kind {
                BpKind::Block(b) => format!("block {} tape {}", b, bp.tape),
                BpKind::Line(n) => format!("line {} tape {}", n, bp.tape),
            };
            let conds = if bp.conditions.is_empty() {
                String::new()
            } else {
                format!(" ({} condition(s))", bp.conditions.len())
            };
            format!("{}: {} [{}]{}", bp.id, kind, status, conds)
        }).collect()
    }

    fn cmd_dump(&self, show_tapes: bool, show_dis: bool) -> Vec<String> {
        if show_tapes {
            return self.cmd_dump_tapes(show_dis);
        }
        let mut lines = Vec::new();
        lines.push("     | 0            1            2            3            4            5            6            7            8            9".to_string());
        lines.push("-----+".to_string() + &"-".repeat(130));
        for row in 1..=9usize {
            let base = row * 10;
            let vals: Vec<String> = (0..10).map(|col| format!("{}", self.machine.stores[base + col - 10])).collect();
            lines.push(format!("{:3}  | {}", base, vals.join("  ")));
        }
        lines.push(String::new());
        lines.push(format!("acc    = {}", self.machine.acc));
        lines.push(format!("sign   = {}", match self.machine.sign_flag {
            None => "(not set)".to_string(),
            Some(true) => "positive".to_string(),
            Some(false) => "negative".to_string(),
        }));
        lines.push(format!("layout = {}", self.machine.layout.map(|n| n.to_string()).unwrap_or_else(|| "(none)".to_string())));
        lines.push(format!("shift  = {}", match self.machine.shift {
            None => "B (default)".to_string(),
            Some(n) => { let l = ['?','A','B','C','D','E','F','G','H','J']; format!("{} pending", l.get(n as usize).unwrap_or(&'?')) }
        }));
        let ip_str = match self.machine.ip {
            IP::Tape { reader, pos } => format!("tape {} pos {}", reader + 1, pos + 1),
            IP::Store(addr) => format!("store {}", addr),
        };
        lines.push(format!("ip     = {}", ip_str));
        lines.push(format!("halted = {}", if self.machine.halted {
            self.machine.halt_reason.as_ref().map(|r| r.to_string()).unwrap_or_else(|| "yes".to_string())
        } else {
            "no".to_string()
        }));
        lines
    }

    fn cmd_dump_tapes(&self, show_dis: bool) -> Vec<String> {
        use crate::tape::TapeEntry;
        let mut lines = Vec::new();
        let mut any = false;
        for tape_num in 1..=7usize {
            let tape = match self.machine.tape_ref(tape_num) {
                Some(t) => t,
                None => continue,
            };
            any = true;
            let cur_pos = if self.machine.active_tape_num() == Some(tape_num) {
                self.machine.current_tape_pos()
            } else {
                None
            };
            lines.push(format!("=== tape {} ({} entries) ===", tape_num, tape.entries.len()));
            for (idx, entry) in tape.entries.iter().enumerate() {
                let marker = if cur_pos == Some(idx) { ">" } else { " " };
                let base = format!("{} {:4}: {}", marker, idx + 1, entry);
                if show_dis {
                    if let TapeEntry::Order(o) = entry {
                        lines.push(format!("{}  {}", base, disassemble(*o)));
                    } else {
                        lines.push(base);
                    }
                } else {
                    lines.push(base);
                }
            }
            lines.push(String::new());
        }
        if !any {
            lines.push("no tapes loaded".to_string());
        }
        lines
    }

    fn cmd_help() -> Vec<String> {
        let cmds: &[(&str, &str)] = &[
            ("run / r",                        "run until halt or Ctrl-C"),
            ("step / s",                       "execute one order"),
            ("skip",                           "advance tape without executing"),
            ("list [tape]",                    "show tape from current position"),
            ("print <loc>",                    "store 10-99, acc, sign, layout, shift"),
            ("dis [n]",                        "disassemble next n orders (default 1)"),
            ("load <file> [tape]",             "load tape file"),
            ("reset",                          "reset machine, keep tapes"),
            ("clear [tape]",                   "unload tape(s)"),
            ("exec <order>",                   "execute 5-digit order without advancing tape"),
            ("transfer <tape>",                "transfer control to tape reader 1-7"),
            ("search <block> [tape]",          "advance tape to block marker 0-9"),
            ("break",                          "list breakpoints"),
            ("break block <b> [t]",            "add breakpoint at block marker b"),
            ("break line <n> [t]",             "add breakpoint at line number n"),
            ("break dis <id>",                 "disable breakpoint"),
            ("break en <id>",                  "enable breakpoint"),
            ("break rm <id>",                  "remove breakpoint"),
            ("break when <id> <loc> [op val]", "add condition to breakpoint"),
            ("dump",                           "show full machine state"),
            ("dump tapes",                     "dump all loaded tape contents"),
            ("dump tapes dis",                 "dump tapes with disassembly"),
            ("quit / exit / q",                "exit"),
        ];
        let width = cmds.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
        let mut out = vec!["Commands:".to_string()];
        for (cmd, desc) in cmds {
            out.push(format!("  {:<width$}  {}", cmd, desc, width = width));
        }
        out
    }

    fn show_position(&self) -> Vec<String> {
        match self.machine.ip {
            IP::Tape { reader, pos } => vec![format!("tape {} line {}", reader + 1, pos + 1)],
            IP::Store(addr) => vec![format!("store {}", addr)],
        }
    }

    fn check_breakpoints(&mut self) -> Option<String> {
        let (tape_num, pos) = match self.machine.ip {
            IP::Tape { reader, pos } => (reader + 1, pos),
            _ => return None,
        };

        for bp in &mut self.breakpoints {
            if !bp.enabled || bp.tape != tape_num {
                continue;
            }
            let tape = self.machine.tapes.get(tape_num - 1)?.as_ref()?;

            let hit = match bp.kind {
                BpKind::Block(b) => {
                    // check if the entry just before current pos is this block marker
                    pos > 0 && matches!(tape.entries.get(pos - 1), Some(TapeEntry::Block(blk)) if *blk == b)
                }
                BpKind::Line(line) => pos + 1 == line,
            };

            if hit {
                let cond_met = bp.conditions.is_empty()
                    || bp.conditions.iter().all(|c| eval_condition(c, &self.machine));
                if cond_met {
                    return Some(format!("breakpoint {} hit at tape {} line {}", bp.id, tape_num, pos + 1));
                }
            }
        }
        None
    }
}

fn eval_condition(cond: &Condition, machine: &Machine) -> bool {
    let current: i64 = match &cond.location {
        Location::Store(addr) => machine.stores[addr - 10].to_i64(),
        Location::Acc => {
            let m = machine.acc.magnitude as i64;
            if machine.acc.negative { -m } else { m }
        }
        Location::Signal | Location::Alarm => 0,
        Location::Sign => machine.sign_flag.map(|b| b as i64).unwrap_or(0),
    };
    match cond.op {
        CondOp::Changes => cond.prev.map(|p| p != current).unwrap_or(false),
        CondOp::Lt => cond.value.map(|v| current < v).unwrap_or(false),
        CondOp::Gt => cond.value.map(|v| current > v).unwrap_or(false),
        CondOp::Eq => cond.value.map(|v| current == v).unwrap_or(false),
        CondOp::Le => cond.value.map(|v| current <= v).unwrap_or(false),
        CondOp::Ge => cond.value.map(|v| current >= v).unwrap_or(false),
        CondOp::Ne => cond.value.map(|v| current != v).unwrap_or(false),
    }
}

fn parse_location(s: &str) -> Result<Location, String> {
    match s {
        "acc" => Ok(Location::Acc),
        "signal" | "sig" => Ok(Location::Signal),
        "alarm" => Ok(Location::Alarm),
        "sign" => Ok(Location::Sign),
        _ => {
            let addr: usize = s.parse().map_err(|_| {
                format!("unknown location '{}' (use acc, sign, signal, alarm, or 10-99)", s)
            })?;
            if !(10..=99).contains(&addr) {
                return Err(format!("store {} out of range (10-99)", addr));
            }
            Ok(Location::Store(addr))
        }
    }
}

fn format_output(o: Output) -> Vec<String> {
    match o {
        Output::Print(s) => vec![s],
        Output::Perforate(s) => vec![format!("[punch] {}", s)],
    }
}
