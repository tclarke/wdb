use crate::disasm::disassemble;
use crate::machine::{Machine, Output, IP};
use crate::tape::{order_entry_idx, order_line_num, parse_tape_file, TapeEntry, WitchAcc, WitchNum};
use colored::Colorize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn use_color() -> bool {
    colored::control::SHOULD_COLORIZE.should_colorize()
}

fn color_num(n: WitchNum) -> String {
    let s = n.to_string();
    if n.magnitude == 0 {
        s
    } else if n.negative {
        s.red().to_string()
    } else {
        s.green().to_string()
    }
}

fn color_acc(n: WitchAcc) -> String {
    let s = n.to_string();
    if n.magnitude == 0 {
        s
    } else if n.negative {
        s.red().to_string()
    } else {
        s.green().to_string()
    }
}

struct MachineSnapshot {
    stores: [WitchNum; 90],
    acc: WitchAcc,
    sign_flag: Option<bool>,
    tape_positions: [Option<usize>; 7],
}

impl MachineSnapshot {
    fn capture(m: &Machine) -> Self {
        MachineSnapshot {
            stores: m.stores,
            acc: m.acc,
            sign_flag: m.sign_flag,
            tape_positions: std::array::from_fn(|i| m.tapes[i].as_ref().map(|t| t.pos)),
        }
    }
}

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

#[derive(Clone)]
enum LastCmd {
    None,
    Step,
    List { tape: usize, next_start: usize, n: usize },
    Dis { next_start: usize, n: usize },
}

pub struct Debugger {
    pub machine: Machine,
    breakpoints: Vec<Breakpoint>,
    next_bp_id: usize,
    autodis: bool,
    autowhat: bool,
    last_cmd: LastCmd,
}

impl Debugger {
    pub fn new() -> Self {
        Debugger {
            machine: Machine::new(),
            breakpoints: Vec::new(),
            next_bp_id: 1,
            autodis: true,
            autowhat: true,
            last_cmd: LastCmd::None,
        }
    }

    /// Execute a debugger command line. Returns lines to print.
    pub fn execute(&mut self, line: &str) -> Vec<String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return self.execute_repeat();
        }

        match parts[0] {
            "run" | "r" => self.cmd_run(),
            "step" | "s" | "next" | "n" => {
                self.last_cmd = LastCmd::Step;
                self.cmd_step()
            }
            "skip" => self.cmd_skip(),
            "list" | "l" => {
                let default_tape = self.machine.active_tape_num().unwrap_or(1);
                let (start, n, tape) = parse_list_args(&parts[1..], default_tape);
                self.cmd_list(start, n, tape)
            }
            "print" | "p" => {
                if parts.len() < 2 {
                    return vec!["usage: print <store|acc|signal|alarm|sign|layout|shift>".to_string()];
                }
                self.cmd_print(parts[1])
            }
            "dis" => {
                let (start, n) = parse_dis_args(&parts[1..]);
                self.cmd_dis(start, n)
            }
            "set" => {
                if parts.len() < 3 {
                    return vec!["usage: set autodis|autowhat true|false".to_string()];
                }
                self.cmd_set(parts[1], parts[2])
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
            "dump" | "d" => {
                let show_tapes = parts.get(1).map(|&s| s == "tapes").unwrap_or(false);
                let show_dis = parts.get(2).map(|&s| s == "dis").unwrap_or(false);
                self.cmd_dump(show_tapes, show_dis)
            }
            "help" | "h" | "?" => Self::cmd_help(),
            "quit" | "exit" | "q" => std::process::exit(0),
            cmd => vec![format!("unknown command '{}' (type 'help' for help)", cmd).red().to_string()],
        }
    }

    // ---- Commands ----

    fn cmd_run(&mut self) -> Vec<String> {
        if self.machine.halted {
            return vec!["machine halted; use 'reset' to restart".yellow().to_string()];
        }
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let _ = ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        });

        let mut output = Vec::new();
        loop {
            if !running.load(Ordering::SeqCst) {
                output.push("^C — interrupted".yellow().to_string());
                break;
            }
            if let Some(msg) = self.check_breakpoints() {
                output.push(msg);
                break;
            }
            match self.machine.step() {
                Ok(outs) => output.extend(outs.into_iter().flat_map(format_output)),
                Err(reason) => {
                    output.extend(self.machine.take_incomplete_line().into_iter().flat_map(format_output));
                    output.push(format!("{} {}", "halted:".bold().red(), reason));
                    break;
                }
            }
        }
        output
    }

    fn cmd_step(&mut self) -> Vec<String> {
        if self.machine.halted {
            return vec!["machine halted; use 'reset' to restart".yellow().to_string()];
        }
        let snap = if self.autowhat { Some(MachineSnapshot::capture(&self.machine)) } else { None };
        let mut out = Vec::new();
        match self.machine.step() {
            Ok(outs) => out.extend(outs.into_iter().flat_map(format_output)),
            Err(reason) => {
                out.extend(self.machine.take_incomplete_line().into_iter().flat_map(format_output));
                out.push(format!("{} {}", "halted:".bold().red(), reason));
                return out;
            }
        }
        if let Some(before) = snap {
            let what = self.format_what_changed(&before);
            if !what.is_empty() {
                out.push(String::new());
                out.extend(what);
            }
        }
        if self.autodis {
            let (dis_out, _) = self.dis_lines(None, 1);
            if !dis_out.is_empty() { out.push(String::new()); }
            out.extend(dis_out);
        }
        out
    }

    fn format_what_changed(&self, before: &MachineSnapshot) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        let m = &self.machine;

        // changed stores
        for i in 0..90usize {
            if m.stores[i] != before.stores[i] {
                let addr = i + 10;
                let old = if use_color() { color_num(before.stores[i]) } else { before.stores[i].to_string() };
                let new = if use_color() { color_num(m.stores[i]) } else { m.stores[i].to_string() };
                parts.push(format!("store {:02}: {} -> {}", addr, old, new));
            }
        }

        // changed acc
        if m.acc != before.acc {
            let old = if use_color() { color_acc(before.acc) } else { before.acc.to_string() };
            let new = if use_color() { color_acc(m.acc) } else { m.acc.to_string() };
            parts.push(format!("acc: {} -> {}", old, new));
        }

        // changed sign flag
        if m.sign_flag != before.sign_flag {
            let fmt_flag = |f: Option<bool>| match f {
                None => "unset".to_string(),
                Some(true) => "+".to_string(),
                Some(false) => "-".to_string(),
            };
            parts.push(format!("sign: {} -> {}", fmt_flag(before.sign_flag), fmt_flag(m.sign_flag)));
        }

        // tape advances (non-IP tapes only — IP advance is expected)
        let active = m.active_tape_num();
        for i in 0..7usize {
            let tape_num = i + 1;
            if Some(tape_num) == active { continue; }
            let old_pos = before.tape_positions[i];
            let new_pos = m.tapes[i].as_ref().map(|t| t.pos);
            if old_pos != new_pos {
                let old_s = old_pos.map(|p| (p + 1).to_string()).unwrap_or_else(|| "?".into());
                let new_s = new_pos.map(|p| (p + 1).to_string()).unwrap_or_else(|| "?".into());
                parts.push(format!("tape {} pos: {} -> {}", tape_num, old_s, new_s));
            }
        }

        if parts.is_empty() {
            return Vec::new();
        }

        if use_color() {
            vec![parts.join("  ").dimmed().to_string()]
        } else {
            vec![parts.join("  ")]
        }
    }

    fn cmd_skip(&mut self) -> Vec<String> {
        match self.machine.skip() {
            Ok(()) => self.show_position(),
            Err(e) => vec![format!("{} {}", "error:".red(), e)],
        }
    }

    fn cmd_list(&mut self, start: Option<usize>, n: usize, tape_num: usize) -> Vec<String> {
        let cur_pos = self.machine.current_tape_pos()
            .filter(|_| self.machine.active_tape_num() == Some(tape_num));

        let (lines, next_start) = {
            let tape = match self.machine.tape_ref(tape_num) {
                Some(t) => t,
                None => return vec![format!("tape {} not loaded or empty", tape_num).red().to_string()],
            };
            if tape.entries.is_empty() {
                return vec![format!("tape {} not loaded or empty", tape_num).red().to_string()];
            }
            let tape_start = start.unwrap_or_else(|| cur_pos.unwrap_or(tape.pos));
            let mut items: Vec<(String, usize, Option<String>)> = Vec::new();
            let mut comment_idx = tape.comments.partition_point(|(before, _)| *before < tape_start);
            let mut entry_count = 0;
            let mut next_start = tape_start;
            for (idx, entry) in tape.entries.iter().enumerate().skip(tape_start) {
                if entry_count >= n {
                    break;
                }
                while comment_idx < tape.comments.len() && tape.comments[comment_idx].0 == idx {
                    let text = format!("      ; {}", tape.comments[comment_idx].1);
                    items.push((text.dimmed().to_string(), 0, None));
                    comment_idx += 1;
                }
                let inline = tape.inline_comments.iter()
                    .find(|(i, _)| *i == idx)
                    .map(|(_, c)| c.clone());
                let at_cur = cur_pos == Some(idx);
                let lnum = if matches!(entry, TapeEntry::Block(_)) { 0 } else { order_line_num(&tape.entries, idx) };
                let lnum_str = if lnum == 0 { "    ".to_string() } else { format!("{:4}", lnum) };
                let base_plain = format!("{} {}: {}", if at_cur { ">" } else { " " }, lnum_str, entry);
                let plain_len = base_plain.len();
                let base = if use_color() {
                    let marker = if at_cur { "▶".bright_cyan().bold().to_string() } else { " ".to_string() };
                    format!("{} {}: {}", marker, lnum_str.dimmed(), entry)
                } else {
                    base_plain
                };
                items.push((base, plain_len, inline));
                entry_count += 1;
                next_start = idx + 1;
            }
            (align_inline_comments(items), next_start)
        };

        self.last_cmd = LastCmd::List { tape: tape_num, next_start, n };
        lines
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

    fn dis_lines(&self, start: Option<usize>, n: usize) -> (Vec<String>, usize) {
        let tape_num = self.machine.active_tape_num().unwrap_or(1);
        let tape = match self.machine.tape_ref(tape_num) {
            Some(t) => t,
            None => return (vec!["no orders at current position".to_string()], 0),
        };
        let tape_start = start.unwrap_or_else(|| self.machine.current_tape_pos().unwrap_or(tape.pos));
        let cur_pos = self.machine.current_tape_pos();
        let inline_comments = &tape.inline_comments;
        let mut items: Vec<(String, usize, Option<String>)> = Vec::new();
        let mut count = 0;
        let mut next_start = tape_start;
        for (idx, entry) in tape.entries.iter().enumerate().skip(tape_start) {
            if count >= n {
                break;
            }
            if let TapeEntry::Order(o) = entry {
                let lnum = order_line_num(&tape.entries, idx);
                let bp = self.bp_marker(tape_num, idx);
                let inline = inline_comments.iter()
                    .find(|(i, _)| *i == idx)
                    .map(|(_, c)| c.clone());
                let at_cur = cur_pos == Some(idx);
                let cur_marker = if at_cur { ">" } else { " " };
                let base_plain = format!("{}{} {:4}: {:05}  {}", bp.plain, cur_marker, lnum, o, disassemble(*o));
                let plain_len = base_plain.len();
                let base = if use_color() {
                    let cur_col = if at_cur { "▶".bright_cyan().bold().to_string() } else { " ".to_string() };
                    format!("{}{} {}: {:05}  {}", bp.colored, cur_col, format!("{:4}", lnum).dimmed(), o, disassemble(*o))
                } else {
                    base_plain
                };
                items.push((base, plain_len, inline));
                count += 1;
                next_start = idx + 1;
            }
        }
        if items.is_empty() {
            return (vec!["no orders at current position".to_string()], tape_start);
        }
        (align_inline_comments(items), next_start)
    }

    fn cmd_dis(&mut self, start: Option<usize>, n: usize) -> Vec<String> {
        let (lines, next_start) = self.dis_lines(start, n);
        self.last_cmd = LastCmd::Dis { next_start, n };
        lines
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
            Err(e) => vec![format!("{} {}", "error:".red(), e)],
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
            _ => return vec![format!("invalid order '{}' (must be 5-digit number 00000-99999)", order_str).red().to_string()],
        };
        match self.machine.exec_single(order) {
            Ok(outs) => outs.into_iter().flat_map(format_output).collect(),
            Err(e) => vec![format!("{} {}", "error:".red(), e)],
        }
    }

    fn cmd_transfer(&mut self, tape_str: &str) -> Vec<String> {
        let n: u8 = match tape_str.parse() {
            Ok(n) if (1..=7).contains(&n) => n,
            _ => return vec![format!("invalid tape number '{}' (must be 1-7)", tape_str).red().to_string()],
        };
        let order = 2 * 10000 + 1000 + n as u32; // 021rr
        match self.machine.exec_single(order) {
            Ok(_) => {
                vec![format!("transferred to tape {}", n)]
            }
            Err(e) => vec![format!("{} {}", "error:".red(), e)],
        }
    }

    fn cmd_search(&mut self, block_str: &str, tape_num: Option<usize>) -> Vec<String> {
        let block: u8 = match block_str.parse::<u8>() {
            Ok(b) if b <= 9 => b,
            _ => return vec![format!("invalid block '{}' (must be 0-9)", block_str).red().to_string()],
        };
        let tape_num = tape_num.unwrap_or_else(|| self.machine.active_tape_num().unwrap_or(1));
        let order = 3 * 10000 + (block as u32) * 100 + tape_num as u32;
        match self.machine.exec_single(order) {
            Ok(_) => vec![format!("tape {} positioned at block {}", tape_num, block)],
            Err(e) => vec![format!("{} {}", "error:".red(), e)],
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
                let entry_idx = match self.machine.tapes.get(tape - 1).and_then(|t| t.as_ref())
                    .and_then(|t| order_entry_idx(&t.entries, lineno))
                {
                    Some(i) => i,
                    None => return vec![format!("line {} not found on tape {}", lineno, tape)],
                };
                let id = self.next_bp_id;
                self.next_bp_id += 1;
                self.breakpoints.push(Breakpoint { id, kind: BpKind::Line(entry_idx), tape, enabled: true, conditions: Vec::new() });
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
                BpKind::Line(entry_idx) => {
                    let lnum = self.machine.tapes.get(bp.tape - 1).and_then(|t| t.as_ref())
                        .map(|t| order_line_num(&t.entries, entry_idx)).unwrap_or(entry_idx + 1);
                    format!("line {} tape {}", lnum, bp.tape)
                }
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
        let pipe = if use_color() { "┃" } else { "|" };
        let sep = if use_color() { "━".repeat(130) } else { "-".repeat(130) };
        lines.push(format!("     {} 0            1            2            3            4            5            6            7            8            9", pipe).dimmed().to_string());
        lines.push(format!("-----+{}", sep).dimmed().to_string());
        for row in 1..=9usize {
            let base = row * 10;
            let vals: Vec<String> = (0..10).map(|col| color_num(self.machine.stores[base + col - 10])).collect();
            lines.push(format!("{:3}  {} {}", base, pipe, vals.join("  ")));
        }
        lines.push(String::new());
        lines.push(format!("{} {}", "acc    =".dimmed(), self.machine.acc));
        lines.push(format!("{} {}", "sign   =".dimmed(), match self.machine.sign_flag {
            None => "(not set)".to_string(),
            Some(true) => "positive".to_string(),
            Some(false) => "negative".to_string(),
        }));
        lines.push(format!("{} {}", "layout =".dimmed(), self.machine.layout.map(|n| n.to_string()).unwrap_or_else(|| "(none)".to_string())));
        lines.push(format!("{} {}", "shift  =".dimmed(), match self.machine.shift {
            None => "B (default)".to_string(),
            Some(n) => { let l = ['?','A','B','C','D','E','F','G','H','J']; format!("{} pending", l.get(n as usize).unwrap_or(&'?')) }
        }));
        let ip_str = match self.machine.ip {
            IP::Tape { reader, pos } => format!("tape {} pos {}", reader + 1, pos + 1),
            IP::Store(addr) => format!("store {}", addr),
        };
        lines.push(format!("{} {}", "ip     =".dimmed(), ip_str));
        let halted_val = if self.machine.halted {
            let s = self.machine.halt_reason.as_ref().map(|r| r.to_string()).unwrap_or_else(|| "yes".to_string());
            s.red().to_string()
        } else {
            "no".to_string()
        };
        lines.push(format!("{} {}", "halted =".dimmed(), halted_val));
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
            lines.push(format!("=== tape {} ({} entries) ===", tape_num, tape.entries.len()).bold().to_string());
            let mut items: Vec<(String, usize, Option<String>)> = Vec::new();
            let mut comment_idx = 0usize;
            for (idx, entry) in tape.entries.iter().enumerate() {
                while comment_idx < tape.comments.len() && tape.comments[comment_idx].0 == idx {
                    let text = format!("      ; {}", tape.comments[comment_idx].1);
                    items.push((text.dimmed().to_string(), 0, None));
                    comment_idx += 1;
                }
                let inline = tape.inline_comments.iter()
                    .find(|(i, _)| *i == idx)
                    .map(|(_, c)| c.clone());
                let at_cur = cur_pos == Some(idx);
                let lnum = if matches!(entry, TapeEntry::Block(_)) { 0 } else { order_line_num(&tape.entries, idx) };
                let lnum_str = if lnum == 0 { "    ".to_string() } else { format!("{:4}", lnum) };
                let base_plain = format!("{} {}: {}", if at_cur { ">" } else { " " }, lnum_str, entry);
                let plain_len = base_plain.len();
                let base = if use_color() {
                    let marker = if at_cur { "▶".bright_cyan().bold().to_string() } else { " ".to_string() };
                    format!("{} {}: {}", marker, lnum_str.dimmed(), entry)
                } else {
                    base_plain
                };
                if show_dis {
                    if let TapeEntry::Order(o) = entry {
                        let with_dis_plain = format!("{}  {}", if at_cur { format!("> {}: {}", lnum_str, entry) } else { format!("  {}: {}", lnum_str, entry) }, disassemble(*o));
                        let plain_len_dis = with_dis_plain.len();
                        let base_dis = format!("{}  {}", base, disassemble(*o));
                        items.push((base_dis, plain_len_dis, inline));
                    } else {
                        items.push((base, plain_len, inline));
                    }
                } else {
                    items.push((base, plain_len, inline));
                }
            }
            // trailing comments after all entries
            while comment_idx < tape.comments.len() {
                let text = format!("      ; {}", tape.comments[comment_idx].1);
                items.push((text.dimmed().to_string(), 0, None));
                comment_idx += 1;
            }
            lines.extend(align_inline_comments(items));
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
            ("step / s / next / n",            "execute one order"),
            ("skip",                           "advance tape without executing"),
            ("list [L:N] [tape]",              "show N entries from line L (default: 4 from current)"),
            ("print <loc>",                    "store 10-99, acc, sign, layout, shift"),
            ("dis [L:N]",                      "disassemble N orders from line L (default: 4 from current)"),
            ("set autodis true|false",         "auto-disassemble next instruction after step"),
            ("set autowhat true|false",        "show changed stores/acc/flags/tape positions after step"),
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
            ("dump / d",                       "show full machine state"),
            ("dump tapes",                     "dump all loaded tape contents"),
            ("dump tapes dis",                 "dump tapes with disassembly"),
            ("quit / exit / q",                "exit"),
            ("<Enter>",                        "repeat last step/list/dis"),
        ];
        let width = cmds.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
        let mut out = vec!["Commands:".to_string()];
        for (cmd, desc) in cmds {
            out.push(format!("  {:<width$}  {}", cmd, desc, width = width));
        }
        out
    }

    fn execute_repeat(&mut self) -> Vec<String> {
        match self.last_cmd.clone() {
            LastCmd::None => Vec::new(),
            LastCmd::Step => self.cmd_step(),
            LastCmd::List { tape, next_start, n } => self.cmd_list(Some(next_start), n, tape),
            LastCmd::Dis { next_start, n } => self.cmd_dis(Some(next_start), n),
        }
    }

    fn cmd_set(&mut self, key: &str, val: &str) -> Vec<String> {
        match key {
            "autodis" => match val {
                "true" | "on" | "1" => { self.autodis = true; vec!["autodis on".to_string()] }
                "false" | "off" | "0" => { self.autodis = false; vec!["autodis off".to_string()] }
                _ => vec![format!("invalid value '{}' (use true/false)", val)],
            },
            "autowhat" => match val {
                "true" | "on" | "1" => { self.autowhat = true; vec!["autowhat on".to_string()] }
                "false" | "off" | "0" => { self.autowhat = false; vec!["autowhat off".to_string()] }
                _ => vec![format!("invalid value '{}' (use true/false)", val)],
            },
            _ => vec![format!("unknown setting '{}' (use: autodis, autowhat)", key)],
        }
    }

    fn bp_marker(&self, tape_num: usize, entry_idx: usize) -> BpMarker {
        let bp = self.breakpoints.iter().find(|b| b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == entry_idx));
        match bp {
            None => BpMarker { plain: " ", colored: " ".to_string() },
            Some(b) if !b.enabled => BpMarker { plain: "-", colored: "○".dimmed().to_string() },
            Some(b) if !b.conditions.is_empty() => BpMarker { plain: "c", colored: "◆".yellow().to_string() },
            Some(_) => BpMarker { plain: "*", colored: "●".bright_red().to_string() },
        }
    }

    fn show_position(&self) -> Vec<String> {
        match self.machine.ip {
            IP::Tape { reader, pos } => {
                let lnum = self.machine.tapes.get(reader).and_then(|t| t.as_ref())
                    .map(|t| order_line_num(&t.entries, pos)).unwrap_or(pos + 1);
                vec![format!("tape {} line {}", reader + 1, lnum)]
            }
            IP::Store(addr) => vec![format!("store {}", addr)],
        }
    }

    pub fn breakpoints(&self) -> &[Breakpoint] {
        &self.breakpoints
    }

    pub fn check_break(&mut self) -> Option<String> {
        self.check_breakpoints()
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
                BpKind::Line(entry_idx) => pos == entry_idx,
            };

            if hit {
                let cond_met = bp.conditions.is_empty()
                    || bp.conditions.iter().all(|c| eval_condition(c, &self.machine));
                if cond_met {
                    let lnum = order_line_num(&tape.entries, pos);
                    return Some(format!("breakpoint {} hit at tape {} line {}", bp.id, tape_num, lnum).bold().yellow().to_string());
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
    let pipe = if use_color() { "│" } else { "|" };
    match o {
        Output::Print(dst, s) => vec![format!("{}{} ", format!("{:02}", dst).dimmed(), pipe.dimmed()) + &s],
        Output::Perforate(dst, s) => vec![format!("{}{} ", format!("{:02}", dst).dimmed(), pipe.dimmed()) + &s],
    }
}

struct BpMarker { plain: &'static str, colored: String }

fn parse_list_args(args: &[&str], default_tape: usize) -> (Option<usize>, usize, usize) {
    if args.is_empty() {
        return (None, 4, default_tape);
    }
    if let Some((l_str, n_str)) = args[0].split_once(':') {
        let start = l_str.parse::<usize>().ok().map(|n| n.saturating_sub(1));
        let n = if n_str.is_empty() { 4 } else { n_str.parse().unwrap_or(4) };
        let tape = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(default_tape);
        (start, n, tape)
    } else if let Ok(tape) = args[0].parse::<usize>() {
        (None, 4, tape)
    } else {
        (None, 4, default_tape)
    }
}

fn parse_dis_args(args: &[&str]) -> (Option<usize>, usize) {
    if args.is_empty() {
        return (None, 4);
    }
    if let Some((l_str, n_str)) = args[0].split_once(':') {
        let start = l_str.parse::<usize>().ok().map(|n| n.saturating_sub(1));
        let n = if n_str.is_empty() { 4 } else { n_str.parse().unwrap_or(4) };
        (start, n)
    } else if let Ok(n) = args[0].parse::<usize>() {
        (None, n)
    } else {
        (None, 4)
    }
}

// plain_len = display width of base (no ANSI bytes) for correct column alignment
fn align_inline_comments(items: Vec<(String, usize, Option<String>)>) -> Vec<String> {
    let col = items.iter()
        .filter(|(_, _, c)| c.is_some())
        .map(|(_, plain_len, _)| plain_len + 4)
        .max()
        .unwrap_or(0);
    items.into_iter().map(|(base, plain_len, comment)| match comment {
        None => base,
        Some(c) => {
            let pad = col.saturating_sub(plain_len);
            let comment_str = format!("; {}", c).dimmed().to_string();
            format!("{}{:pad$}{}", base, "", comment_str, pad = pad)
        }
    }).collect()
}
