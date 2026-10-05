use std::collections::VecDeque;
use std::error::Error;
use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Terminal;

use crate::debugger::{BpKind, Breakpoint, Debugger};
use crate::machine::{Machine, Output, IP};
use crate::tape::{TapeEntry, WitchAcc, WitchNum};
use crate::disasm::disassemble;

const LOG_CAP: usize = 1000;
const STEPS_PER_TICK: usize = 200;

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Tape(usize), // 0-based tape index
    State,
    Dis,
    Log,
}

#[derive(Clone)]
enum EditTarget {
    TapeEntry { tape: usize, idx: usize },
    Store { addr: usize },
    Acc,
}

#[derive(Clone)]
enum EditError {
    None,
    Bad(String),
}

struct App {
    debugger: Debugger,
    focus: Focus,
    tape_cursor: [usize; 7],
    tape_scroll: [usize; 7],
    state_scroll: usize,
    state_hscroll: usize,
    dis_scroll: usize,
    dis_hscroll: usize,
    dis_cursor: usize,
    log_scroll: usize,
    tape_visible: [bool; 7],
    show_dis: bool,
    show_state: bool,
    show_output: bool,
    running: bool,
    log: VecDeque<String>,
    // edit mode
    edit: Option<EditTarget>,
    edit_buf: String,
    edit_err: EditError,
    // cli drop mode
    cli_mode: bool,
    cli_buf: String,
    // load prompt: Some(Some(tape_idx)) = load into tape, Some(None) = load all
    load_prompt: Option<Option<usize>>,
    load_prompt_buf: String,
}

impl App {
    fn new(debugger: Debugger) -> Self {
        App {
            debugger,
            focus: Focus::Tape(0),
            tape_cursor: [0; 7],
            tape_scroll: [0; 7],
            state_scroll: 0,
            state_hscroll: 0,
            dis_scroll: 0,
            dis_hscroll: 0,
            dis_cursor: 0,
            log_scroll: 0,
            show_dis: true,
            show_state: true,
            show_output: true,
            running: false,
            log: VecDeque::with_capacity(LOG_CAP),
            edit: None,
            edit_buf: String::new(),
            edit_err: EditError::None,
            cli_mode: false,
            cli_buf: String::new(),
            tape_visible: [false; 7],
            load_prompt: None,
            load_prompt_buf: String::new(),
        }
    }

    fn push_log(&mut self, s: impl Into<String>) {
        if self.log.len() >= LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(s.into());
    }

    fn push_outputs(&mut self, outs: Vec<Output>) {
        for o in outs {
            let line = match o {
                Output::Print(dst, s) => format!("{:02}| {}", dst, s.trim_end_matches('\n')),
                Output::Perforate(dst, s) => format!("{:02}~ {}", dst, s.trim_end_matches('\n')),
            };
            self.push_log(line);
        }
    }

    #[allow(dead_code)]
    fn active_tape(&self) -> Option<usize> {
        if let Focus::Tape(i) = self.focus { Some(i) } else { None }
    }

    fn step_once(&mut self) {
        if let Some(msg) = self.debugger.check_break() {
            self.running = false;
            self.push_log(msg);
            return;
        }
        match self.debugger.machine.step() {
            Ok(outs) => self.push_outputs(outs),
            Err(reason) => {
                if let Some(out) = self.debugger.machine.take_incomplete_line() {
                    self.push_outputs(vec![out]);
                }
                self.running = false;
                self.push_log(format!("halted: {}", reason));
            }
        }
    }

    fn do_step_cmd(&mut self) {
        let outs = self.debugger.execute("step");
        for l in outs {
            if !l.trim().is_empty() {
                self.push_log(l);
            }
        }
        // sync dis_cursor to new IP if dis pane is not being manually navigated
        if let Some(pos) = self.debugger.machine.current_tape_pos() {
            self.dis_cursor = pos;
            if pos < self.dis_scroll {
                self.dis_scroll = pos;
            }
        }
    }

    fn visible_tapes(&self) -> Vec<usize> {
        (0..7).filter(|&i| self.tape_visible[i]).collect()
    }

    fn cycle_focus(&mut self) {
        let visible = self.visible_tapes();
        // build ordered pane list based on what's shown
        let mut panes: Vec<Focus> = visible.iter().map(|&i| Focus::Tape(i)).collect();
        if self.show_state { panes.push(Focus::State); }
        if self.show_dis   { panes.push(Focus::Dis); }
        if self.show_output { panes.push(Focus::Log); }
        if panes.is_empty() { return; }
        let cur_pos = panes.iter().position(|p| p == &self.focus).unwrap_or(0);
        self.focus = panes[(cur_pos + 1) % panes.len()].clone();
    }

    fn scroll_left(&mut self) {
        match self.focus {
            Focus::State => self.state_hscroll = self.state_hscroll.saturating_sub(4),
            Focus::Dis   => self.dis_hscroll   = self.dis_hscroll.saturating_sub(4),
            _ => {}
        }
    }

    fn scroll_right(&mut self) {
        match self.focus {
            Focus::State => self.state_hscroll += 4,
            Focus::Dis   => self.dis_hscroll   += 4,
            _ => {}
        }
    }

    fn scroll_up(&mut self) {
        match self.focus {
            Focus::Tape(i) => {
                if self.tape_cursor[i] > 0 {
                    self.tape_cursor[i] -= 1;
                    if self.tape_cursor[i] < self.tape_scroll[i] {
                        self.tape_scroll[i] = self.tape_cursor[i];
                    }
                }
            }
            Focus::State => self.state_scroll = self.state_scroll.saturating_sub(1),
            Focus::Dis => self.move_dis_cursor(-1),
            Focus::Log => {
                let max = self.log.len().saturating_sub(1);
                self.log_scroll = (self.log_scroll + 1).min(max);
            }
        }
    }

    fn scroll_down(&mut self) {
        match self.focus {
            Focus::Tape(i) => {
                let len = tape_display_len(&self.debugger.machine, i);
                if self.tape_cursor[i] + 1 < len {
                    self.tape_cursor[i] += 1;
                }
            }
            Focus::State => self.state_scroll += 1,
            Focus::Dis => self.move_dis_cursor(1),
            Focus::Log => {
                if self.log_scroll > 0 {
                    self.log_scroll -= 1;
                }
            }
        }
    }

    fn move_dis_cursor(&mut self, delta: i32) {
        let tape_num = match self.debugger.machine.active_tape_num() {
            Some(n) => n,
            None => return,
        };
        let tape = match self.debugger.machine.tape_ref(tape_num) {
            Some(t) => t,
            None => return,
        };
        let order_indices: Vec<usize> = tape.entries.iter().enumerate()
            .filter_map(|(i, e)| if matches!(e, TapeEntry::Order(_)) { Some(i) } else { None })
            .collect();
        if order_indices.is_empty() { return; }
        let cur = self.dis_cursor;
        // find position of cursor in order_indices (nearest match)
        let pos = order_indices.partition_point(|&i| i < cur);
        let pos = pos.min(order_indices.len() - 1);
        let new_pos = if delta < 0 {
            pos.saturating_sub(1)
        } else {
            (pos + 1).min(order_indices.len() - 1)
        };
        self.dis_cursor = order_indices[new_pos];
        // dis_scroll is the entry index to start rendering from; adjust to keep cursor visible
        // (height unknown here — just ensure scroll <= cursor)
        if self.dis_cursor < self.dis_scroll {
            self.dis_scroll = self.dis_cursor;
        }
    }

    #[allow(dead_code)]
    fn bp_at_cursor(&self, tape_idx: usize) -> Option<&Breakpoint> {
        let cursor = self.tape_cursor[tape_idx];
        let tape_num = tape_idx + 1;
        // cursor is over display rows; find entry index at cursor
        let entry_idx = display_row_to_entry(&self.debugger.machine, tape_idx, cursor)?;
        let line_1indexed = entry_idx + 1;
        self.debugger.breakpoints().iter().find(|b| {
            b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == line_1indexed)
        })
    }

    fn toggle_bp(&mut self) {
        let (tape_num, line_1indexed) = match self.focus {
            Focus::Tape(i) => {
                let cursor = self.tape_cursor[i];
                let entry_idx = match display_row_to_entry(&self.debugger.machine, i, cursor) {
                    Some(idx) => idx,
                    None => return,
                };
                (i + 1, entry_idx + 1)
            }
            Focus::Dis => {
                let tape_num = match self.debugger.machine.active_tape_num() {
                    Some(n) => n,
                    None => return,
                };
                (tape_num, self.dis_cursor + 1)
            }
            _ => return,
        };
        // check if bp already exists
        let existing = self.debugger.breakpoints().iter().find(|b| {
            b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == line_1indexed)
        }).map(|b| b.id);
        if let Some(id) = existing {
            let outs = self.debugger.execute(&format!("break rm {}", id));
            for l in outs { self.push_log(l); }
        } else {
            let outs = self.debugger.execute(&format!("break line {} {}", line_1indexed, tape_num));
            for l in outs { self.push_log(l); }
        }
    }

    fn delete_tape_entry(&mut self) {
        let tape_idx = match self.focus {
            Focus::Tape(i) => i,
            _ => return,
        };
        let cursor = self.tape_cursor[tape_idx];
        let entry_idx = match display_row_to_entry(&self.debugger.machine, tape_idx, cursor) {
            Some(i) => i,
            None => return,
        };
        if let Some(Some(tape)) = self.debugger.machine.tapes.get_mut(tape_idx) {
            if entry_idx < tape.entries.len() {
                tape.entries.remove(entry_idx);
                if tape.pos > entry_idx && tape.pos > 0 {
                    tape.pos -= 1;
                }
                // sync IP if this tape is active
                if let IP::Tape { reader, ref mut pos } = self.debugger.machine.ip {
                    if reader == tape_idx {
                        *pos = self.debugger.machine.tapes[tape_idx]
                            .as_ref().map(|t| t.pos).unwrap_or(0);
                    }
                }
                if self.tape_cursor[tape_idx] > 0 {
                    self.tape_cursor[tape_idx] -= 1;
                }
            }
        }
    }

    fn insert_tape_entry(&mut self, after: bool) {
        let tape_idx = match self.focus {
            Focus::Tape(i) => i,
            _ => return,
        };
        let cursor = self.tape_cursor[tape_idx];
        let entry_idx = display_row_to_entry(&self.debugger.machine, tape_idx, cursor)
            .unwrap_or(0);
        let insert_at = if after { entry_idx + 1 } else { entry_idx };
        if let Some(Some(tape)) = self.debugger.machine.tapes.get_mut(tape_idx) {
            let insert_at = insert_at.min(tape.entries.len());
            tape.entries.insert(insert_at, TapeEntry::Order(0));
            if after {
                self.tape_cursor[tape_idx] = cursor + 1;
            }
        }
        self.begin_edit_tape(tape_idx);
    }

    fn begin_edit_tape(&mut self, tape_idx: usize) {
        let cursor = self.tape_cursor[tape_idx];
        let entry_idx = match display_row_to_entry(&self.debugger.machine, tape_idx, cursor) {
            Some(i) => i,
            None => return,
        };
        let prefill = self.debugger.machine.tapes[tape_idx]
            .as_ref()
            .and_then(|t| t.entries.get(entry_idx))
            .map(|e| match e {
                TapeEntry::Order(o) => format!("{:05}", o),
                TapeEntry::Number(n) => n.to_string(),
                TapeEntry::Block(b) => format!("[{}]", b),
            })
            .unwrap_or_default();
        self.edit = Some(EditTarget::TapeEntry { tape: tape_idx, idx: entry_idx });
        self.edit_buf = prefill;
        self.edit_err = EditError::None;
    }

    fn begin_edit_store(&mut self, addr: usize) {
        let val = if (10..=99).contains(&addr) {
            self.debugger.machine.stores[addr - 10].to_string()
        } else if addr == 8 || addr == 9 {
            self.debugger.machine.acc.to_string()
        } else {
            return;
        };
        self.edit = Some(if addr < 10 { EditTarget::Acc } else { EditTarget::Store { addr } });
        self.edit_buf = val;
        self.edit_err = EditError::None;
    }

    fn confirm_edit(&mut self) {
        let target = match self.edit.clone() {
            Some(t) => t,
            None => return,
        };
        match target {
            EditTarget::TapeEntry { tape, idx } => {
                let s = self.edit_buf.trim().to_string();
                match parse_tape_entry_str(&s) {
                    Ok(entry) => {
                        if let Some(Some(t)) = self.debugger.machine.tapes.get_mut(tape) {
                            if idx < t.entries.len() {
                                t.entries[idx] = entry;
                            }
                        }
                        self.edit = None;
                        self.edit_err = EditError::None;
                    }
                    Err(e) => self.edit_err = EditError::Bad(e),
                }
            }
            EditTarget::Store { addr } => {
                match WitchNum::from_display_str(&self.edit_buf) {
                    Some(n) if (10..=99).contains(&addr) => {
                        self.debugger.machine.stores[addr - 10] = n;
                        self.edit = None;
                        self.edit_err = EditError::None;
                    }
                    _ => self.edit_err = EditError::Bad("invalid number (use ±D.DDDDDDD)".into()),
                }
            }
            EditTarget::Acc => {
                match WitchAcc::from_display_str(&self.edit_buf) {
                    Some(a) => {
                        self.debugger.machine.acc = a;
                        self.edit = None;
                        self.edit_err = EditError::None;
                    }
                    None => self.edit_err = EditError::Bad("invalid acc (use ±D.DDDDDDDDDDDDDDD)".into()),
                }
            }
        }
    }

    fn handle_event(&mut self, ev: Event) -> bool {
        if self.load_prompt.is_some() {
            return self.handle_load_prompt_event(ev);
        }
        if self.cli_mode {
            return self.handle_cli_event(ev);
        }
        if self.edit.is_some() {
            return self.handle_edit_event(ev);
        }
        self.handle_normal_event(ev)
    }

    fn handle_normal_event(&mut self, ev: Event) -> bool {
        let Event::Key(kev) = ev else { return false; };
        if kev.modifiers == KeyModifiers::CONTROL && kev.code == KeyCode::Char('c') {
            self.running = false;
            return false;
        }
        match kev.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char(' ') => {
                if self.debugger.machine.halted {
                    self.push_log("halted — use R to reset".to_string());
                } else {
                    self.running = !self.running;
                }
            }
            KeyCode::Char('n') => {
                self.running = false;
                self.do_step_cmd();
            }
            KeyCode::Char('R') => {
                let outs = self.debugger.execute("reset");
                for l in outs { self.push_log(l); }
                self.running = false;
            }
            KeyCode::Char('D') => self.show_dis = !self.show_dis,
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(),
            KeyCode::Left => self.scroll_left(),
            KeyCode::Right => self.scroll_right(),
            // h/l: horizontal scroll in state/dis, otherwise left/right navigation
            KeyCode::Char('h') => {
                if matches!(self.focus, Focus::State | Focus::Dis) {
                    self.scroll_left();
                }
            }
            KeyCode::Char('l') if matches!(self.focus, Focus::State | Focus::Dis) => self.scroll_right(),
            KeyCode::Char('g') => match self.focus {
                Focus::Tape(i) => { self.tape_cursor[i] = 0; self.tape_scroll[i] = 0; }
                Focus::State => self.state_scroll = 0,
                Focus::Dis => self.dis_scroll = 0,
                Focus::Log => self.log_scroll = self.log.len().saturating_sub(1),
            },
            KeyCode::Char('G') => match self.focus {
                Focus::Tape(i) => {
                    let len = tape_display_len(&self.debugger.machine, i);
                    self.tape_cursor[i] = len.saturating_sub(1);
                }
                Focus::State => self.state_scroll = 100,
                Focus::Dis => self.dis_scroll = 100,
                Focus::Log => self.log_scroll = 0,
            },
            KeyCode::Enter => {
                match self.focus {
                    Focus::Tape(i) => self.begin_edit_tape(i),
                    Focus::State => {
                        // edit store at current cursor row
                        let addr = 10 + self.state_scroll.min(89);
                        self.begin_edit_store(addr);
                    }
                    _ => {}
                }
            }
            KeyCode::Delete | KeyCode::Char('x') => {
                if matches!(self.focus, Focus::Tape(_)) {
                    self.delete_tape_entry();
                }
            }
            KeyCode::Char('i') => {
                if matches!(self.focus, Focus::Tape(_)) {
                    self.insert_tape_entry(false);
                }
            }
            KeyCode::Char('a') => {
                if matches!(self.focus, Focus::Tape(_)) {
                    self.insert_tape_entry(true);
                }
            }
            KeyCode::Char('b') => self.toggle_bp(),
            KeyCode::Char('`') => self.cli_mode = true,
            // show/hide panes
            KeyCode::Char(c @ '1'..='7') => {
                let idx = c as usize - '1' as usize;
                self.tape_visible[idx] = !self.tape_visible[idx];
                if !self.tape_visible[idx] && self.focus == Focus::Tape(idx) {
                    self.cycle_focus();
                }
            }
            KeyCode::Char('S') => {
                self.show_state = !self.show_state;
                if !self.show_state && self.focus == Focus::State {
                    self.cycle_focus();
                }
            }
            KeyCode::Char('O') => {
                self.show_output = !self.show_output;
                if !self.show_output && self.focus == Focus::Log {
                    self.cycle_focus();
                }
            }
            // load tape(s)
            KeyCode::Char('l') => {
                let tape_idx = match self.focus {
                    Focus::Tape(i) => i,
                    _ => self.visible_tapes().first().copied().unwrap_or(0),
                };
                self.load_prompt = Some(Some(tape_idx));
                self.load_prompt_buf.clear();
            }
            KeyCode::Char('L') => {
                self.load_prompt = Some(None);
                self.load_prompt_buf.clear();
            }
            // unload tape(s)
            KeyCode::Char('u') => {
                if let Focus::Tape(tape_idx) = self.focus {
                    self.debugger.machine.tapes[tape_idx] = None;
                    self.tape_visible[tape_idx] = false;
                    self.push_log(format!("tape {} unloaded", tape_idx + 1));
                    self.cycle_focus();
                }
            }
            KeyCode::Char('U') => {
                for i in 0..7 {
                    self.debugger.machine.tapes[i] = None;
                    self.tape_visible[i] = false;
                }
                self.push_log("all tapes unloaded");
                if let Focus::Tape(_) = self.focus { self.focus = Focus::State; }
            }
            _ => {}
        }
        false
    }

    fn handle_load_prompt_event(&mut self, ev: Event) -> bool {
        let Event::Key(kev) = ev else { return false; };
        match kev.code {
            KeyCode::Esc => {
                self.load_prompt = None;
                self.load_prompt_buf.clear();
            }
            KeyCode::Tab | KeyCode::Char('\t') => {
                let (completed, candidates) = tab_complete_path(&self.load_prompt_buf);
                if let Some(new_buf) = completed {
                    self.load_prompt_buf = new_buf;
                } else if !candidates.is_empty() {
                    self.push_log(candidates.join("  "));
                }
            }
            KeyCode::Backspace => { self.load_prompt_buf.pop(); }
            KeyCode::Enter => {
                let path = self.load_prompt_buf.trim().to_string();
                let target = self.load_prompt.take();
                self.load_prompt_buf.clear();
                if !path.is_empty() {
                    match target {
                        Some(Some(tape_idx)) => {
                            let tape_num = tape_idx + 1;
                            let outs = self.debugger.execute(&format!("load {} {}", path, tape_num));
                            for l in outs { self.push_log(l); }
                            if self.debugger.machine.tapes[tape_idx].is_some() {
                                self.tape_visible[tape_idx] = true;
                            }
                        }
                        Some(None) => {
                            let outs = self.debugger.execute(&format!("load {}", path));
                            for l in outs { self.push_log(l); }
                            for i in 0..7 {
                                if self.debugger.machine.tapes[i].is_some() {
                                    self.tape_visible[i] = true;
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            KeyCode::Char(c) => self.load_prompt_buf.push(c),
            _ => {}
        }
        false
    }

    fn handle_edit_event(&mut self, ev: Event) -> bool {
        let Event::Key(kev) = ev else { return false; };
        match kev.code {
            KeyCode::Esc => {
                self.edit = None;
                self.edit_err = EditError::None;
            }
            KeyCode::Enter => self.confirm_edit(),
            KeyCode::Backspace => { self.edit_buf.pop(); self.edit_err = EditError::None; }
            KeyCode::Char(c) => { self.edit_buf.push(c); self.edit_err = EditError::None; }
            _ => {}
        }
        false
    }

    fn handle_cli_event(&mut self, ev: Event) -> bool {
        let Event::Key(kev) = ev else { return false; };
        match kev.code {
            KeyCode::Char('`') => {
                self.cli_mode = false;
                self.cli_buf.clear();
            }
            KeyCode::Esc => {
                self.cli_mode = false;
                self.cli_buf.clear();
            }
            KeyCode::Enter => {
                let cmd = self.cli_buf.trim().to_string();
                if cmd == "quit" || cmd == "exit" || cmd == "q" {
                    return true;
                }
                self.push_log(format!("> {}", cmd));
                let outs = self.debugger.execute(&cmd);
                for l in outs {
                    if !l.trim().is_empty() {
                        self.push_log(l);
                    }
                }
                self.cli_buf.clear();
            }
            KeyCode::Backspace => { self.cli_buf.pop(); }
            KeyCode::Char(c) => self.cli_buf.push(c),
            _ => {}
        }
        false
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Total number of display rows for a tape (entries + standalone comments).
fn tape_display_len(machine: &Machine, tape_idx: usize) -> usize {
    machine.tapes[tape_idx].as_ref().map(|t| {
        t.entries.len() + t.comments.len()
    }).unwrap_or(0)
}

/// Map a display row (cursor) to an entry index, skipping comment rows.
fn display_row_to_entry(machine: &Machine, tape_idx: usize, display_row: usize) -> Option<usize> {
    let tape = machine.tapes[tape_idx].as_ref()?;
    let mut row = 0usize;
    let mut comment_idx = 0usize;
    for (entry_idx, _) in tape.entries.iter().enumerate() {
        while comment_idx < tape.comments.len() && tape.comments[comment_idx].0 == entry_idx {
            if row == display_row {
                return None; // cursor is on a comment row
            }
            row += 1;
            comment_idx += 1;
        }
        if row == display_row {
            return Some(entry_idx);
        }
        row += 1;
    }
    None
}

/// Build display rows for a tape as (display_row, is_comment, entry_idx_or_none, text).
fn build_tape_rows(
    machine: &Machine,
    tape_idx: usize,
    ip_pos: Option<usize>,
    bps: &[Breakpoint],
) -> Vec<TapeRow> {
    let tape_num = tape_idx + 1;
    let tape = match machine.tapes[tape_idx].as_ref() {
        Some(t) => t,
        None => return vec![],
    };
    let mut rows = Vec::new();
    let mut comment_idx = 0usize;
    for (entry_idx, entry) in tape.entries.iter().enumerate() {
        while comment_idx < tape.comments.len() && tape.comments[comment_idx].0 == entry_idx {
            rows.push(TapeRow::Comment(tape.comments[comment_idx].1.clone()));
            comment_idx += 1;
        }
        let at_cur = ip_pos == Some(entry_idx);
        let bp = bps.iter().find(|b| {
            b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == entry_idx + 1)
        });
        let inline = tape.inline_comments.iter()
            .find(|(i, _)| *i == entry_idx)
            .map(|(_, c)| c.clone());
        rows.push(TapeRow::Entry { entry_idx, entry: entry.clone(), at_cur, bp_id: bp.map(|b| b.id), bp_enabled: bp.map(|b| b.enabled), inline });
    }
    rows
}

enum TapeRow {
    Comment(String),
    Entry {
        entry_idx: usize,
        entry: TapeEntry,
        at_cur: bool,
        bp_id: Option<usize>,
        bp_enabled: Option<bool>,
        inline: Option<String>,
    },
}

/// Returns (completed_buf, candidates).
/// completed_buf is Some when the buf can be extended; candidates is the full match list.
fn tab_complete_path(buf: &str) -> (Option<String>, Vec<String>) {
    use std::path::Path;
    let (dir_part, file_prefix): (&str, &str) = if buf.ends_with('/') {
        (buf, "")
    } else {
        let p = Path::new(buf);
        let dir = p.parent().and_then(|d| d.to_str()).filter(|d| !d.is_empty()).unwrap_or(".");
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        (dir, name)
    };
    let dir_search = if dir_part.is_empty() { "." } else { dir_part };
    let Ok(rd) = std::fs::read_dir(dir_search) else { return (None, vec![]); };
    let mut matches: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with(file_prefix) { Some(name) } else { None }
        })
        .collect();
    matches.sort();
    if matches.is_empty() { return (None, vec![]); }

    // common prefix of all matches
    let first = &matches[0];
    let mut len = first.len();
    for s in &matches[1..] {
        len = len.min(s.len());
        for (i, (a, b)) in first.bytes().zip(s.bytes()).enumerate() {
            if a != b { len = len.min(i); break; }
        }
    }
    // if no progress beyond what's already typed, return candidates only
    if len <= file_prefix.len() { return (None, matches); }

    let completed_name = first[..len].to_string();
    let new_path = if (dir_part == "." && !buf.contains('/')) || dir_part.is_empty() {
        completed_name.clone()
    } else {
        format!("{}/{}", dir_part.trim_end_matches('/'), completed_name)
    };
    // trailing slash for unique directory
    let new_path = if matches.len() == 1 && Path::new(&new_path).is_dir() {
        format!("{}/", new_path)
    } else {
        new_path
    };
    (Some(new_path), matches)
}

fn parse_tape_entry_str(s: &str) -> Result<TapeEntry, String> {
    let s = s.trim();
    // block marker: [d]
    if let Some(inner) = s.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        let b: u8 = inner.trim().parse().map_err(|_| "block must be 0-9".to_string())?;
        if b > 9 { return Err("block must be 0-9".to_string()); }
        return Ok(TapeEntry::Block(b));
    }
    // number: starts with sign and contains '.'
    if s.starts_with('+') || s.starts_with('-') {
        if let Some(n) = WitchNum::from_display_str(s) {
            return Ok(TapeEntry::Number(n));
        }
        return Err("invalid number (use ±D.DDDDDDD)".into());
    }
    // order: parse as u32, 0-99999
    let o: u32 = s.parse().map_err(|_| "expected 5-digit order (00000-99999)".to_string())?;
    if o > 99999 {
        return Err("order must be 00000-99999".to_string());
    }
    Ok(TapeEntry::Order(o))
}

fn bp_marker_char(bp_id: Option<usize>, bp_enabled: Option<bool>) -> (&'static str, Style) {
    match (bp_id, bp_enabled) {
        (None, _) => (" ", Style::default()),
        (Some(_), Some(false)) => ("○", Style::default().fg(Color::DarkGray)),
        (Some(_), _) => ("●", Style::default().fg(Color::Red)),
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();

    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    let main_area = vchunks[0];
    let status_area = vchunks[1];

    let visible = app.visible_tapes();
    let show_tapes = !visible.is_empty();
    let show_mid = app.show_state || app.show_dis;
    let show_log = app.show_output;

    // Build dynamic vertical constraints
    let mut v_constraints: Vec<Constraint> = Vec::new();
    let mut row_tape: Option<usize> = None;
    let mut row_mid: Option<usize> = None;
    let mut row_log: Option<usize> = None;
    if show_tapes { row_tape = Some(v_constraints.len()); v_constraints.push(Constraint::Percentage(35)); }
    if show_mid   { row_mid  = Some(v_constraints.len()); v_constraints.push(Constraint::Percentage(40)); }
    if show_log   { row_log  = Some(v_constraints.len()); v_constraints.push(Constraint::Min(3)); }

    if v_constraints.is_empty() {
        render_status(f, app, status_area);
        return;
    }
    // Last row fills remaining space
    if let Some(last) = v_constraints.last_mut() { *last = Constraint::Min(3); }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(main_area);

    // Tape row
    if let Some(ri) = row_tape {
        let n_cols = visible.len().min(4).max(1);
        let tape_constraints: Vec<Constraint> = (0..n_cols)
            .map(|_| Constraint::Ratio(1, n_cols as u32))
            .collect();
        let tape_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(tape_constraints)
            .split(rows[ri]);
        for (col, &tape_idx) in visible.iter().take(n_cols).enumerate() {
            render_tape_panel(f, app, tape_cols[col], tape_idx, rows[ri].height as usize - 2);
        }
    }

    // Mid row: state and/or dis
    if let Some(ri) = row_mid {
        match (app.show_state, app.show_dis) {
            (true, true) => {
                let mid_cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(62), Constraint::Min(50)])
                    .split(rows[ri]);
                render_state(f, app, mid_cols[0]);
                render_dis(f, app, mid_cols[1]);
            }
            (true, false) => render_state(f, app, rows[ri]),
            (false, true) => render_dis(f, app, rows[ri]),
            (false, false) => {}
        }
    }

    // Log
    if let Some(ri) = row_log { render_log(f, app, rows[ri]); }

    render_status(f, app, status_area);

    if app.edit.is_some() { render_edit_popup(f, app, area); }
}

fn render_tape_panel(f: &mut ratatui::Frame, app: &App, area: Rect, tape_idx: usize, visible_height: usize) {
    let tape_num = tape_idx + 1;
    let focused = app.focus == Focus::Tape(tape_idx);
    let active_tape = app.debugger.machine.active_tape_num();
    let ip_pos = if active_tape == Some(tape_num) {
        app.debugger.machine.current_tape_pos()
    } else {
        None
    };

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else if active_tape == Some(tape_num) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = format!(" Tape {} ", tape_num);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.debugger.machine.tapes[tape_idx].is_none() {
        let p = Paragraph::new("(not loaded)").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    };

    let rows = build_tape_rows(&app.debugger.machine, tape_idx, ip_pos, app.debugger.breakpoints());
    let scroll = app.tape_scroll[tape_idx];
    let cursor = app.tape_cursor[tape_idx];

    // Auto-scroll to keep cursor visible
    let scroll = {
        let mut s = scroll;
        if cursor < s { s = cursor; }
        if cursor >= s + visible_height { s = cursor + 1 - visible_height; }
        s
    };

    let items: Vec<ListItem> = rows.iter().enumerate().skip(scroll).take(visible_height)
        .map(|(row_idx, row)| {
            let selected = focused && row_idx == cursor;
            match row {
                TapeRow::Comment(text) => {
                    let line = Line::from(Span::styled(
                        format!("  ; {}", text),
                        Style::default().fg(Color::DarkGray),
                    ));
                    let item = ListItem::new(line);
                    if selected { item.style(Style::default().add_modifier(Modifier::REVERSED)) } else { item }
                }
                TapeRow::Entry { entry_idx, entry, at_cur, bp_id, bp_enabled, inline } => {
                    let (bp_char, bp_style) = bp_marker_char(*bp_id, *bp_enabled);
                    let cur_char = if *at_cur { "▶" } else { " " };
                    let cur_style = if *at_cur {
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    let num_style = Style::default().fg(Color::DarkGray);
                    let entry_text = entry.to_string();
                    let mut spans = vec![
                        Span::styled(bp_char, bp_style),
                        Span::styled(cur_char, cur_style),
                        Span::styled(format!("{:4}: ", entry_idx + 1), num_style),
                        Span::raw(entry_text),
                    ];
                    if let Some(comment) = inline {
                        spans.push(Span::styled(
                            format!("  ; {}", comment),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    let line = Line::from(spans);
                    let item = ListItem::new(line);
                    if selected { item.style(Style::default().add_modifier(Modifier::REVERSED)) } else { item }
                }
            }
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

fn render_state(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::State;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Machine State ")
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let m = &app.debugger.machine;
    let ip_str = match m.ip {
        IP::Tape { reader, pos } => format!("tape {} pos {}", reader + 1, pos + 1),
        IP::Store(addr) => format!("store {}", addr),
    };
    let halt_str = if m.halted {
        m.halt_reason.as_ref().map(|r| r.to_string()).unwrap_or_else(|| "yes".into())
    } else {
        "no".into()
    };
    let sign_str: String = match m.sign_flag {
        None => "(unset)".into(),
        Some(true) => "+".into(),
        Some(false) => "-".into(),
    };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("acc:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(m.acc.to_string()),
        ]),
        Line::from(vec![
            Span::styled("sign: ", Style::default().fg(Color::DarkGray)),
            Span::raw(sign_str),
        ]),
        Line::from(vec![
            Span::styled("ip:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(ip_str),
        ]),
        Line::from(vec![
            Span::styled("halt: ", Style::default().fg(Color::DarkGray)),
            Span::styled(halt_str, if m.halted { Style::default().fg(Color::Red) } else { Style::default() }),
        ]),
        Line::from(Span::styled("────── stores ──────", Style::default().fg(Color::DarkGray))),
    ];

    // Store grid: one row per tens-decade (10-19, 20-29, ..., 90-99)
    for decade in 1..=9usize {
        let base = decade * 10;
        let selected_decade = focused && app.state_scroll / 10 == decade;
        let mut spans = vec![Span::styled(
            format!("{:2}: ", base),
            Style::default().fg(Color::DarkGray),
        )];
        for col in 0..10usize {
            let addr = base + col;
            if addr > 99 { break; }
            let val = m.stores[addr - 10];
            let style = if val.negative && val.magnitude > 0 {
                Style::default().fg(Color::Red)
            } else if val.magnitude > 0 {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let text = format!("{:>12}", val.to_string());
            if selected_decade && (app.state_scroll % 10) == col {
                spans.push(Span::styled(text, style.add_modifier(Modifier::REVERSED)));
            } else {
                spans.push(Span::styled(text, style));
            }
            if col < 9 { spans.push(Span::raw(" ")); }
        }
        lines.push(Line::from(spans));
    }

    let scroll_offset = app.state_scroll.min(lines.len().saturating_sub(1)) as u16;
    let p = Paragraph::new(Text::from(lines))
        .scroll((scroll_offset, app.state_hscroll as u16));
    f.render_widget(p, inner);
}

fn render_dis(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Dis;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Disassembly ")
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block.clone(), area);

    let m = &app.debugger.machine;
    let tape_num = m.active_tape_num().unwrap_or(1);
    let tape = match m.tape_ref(tape_num) {
        Some(t) => t,
        None => {
            f.render_widget(Paragraph::new("(no tape)").style(Style::default().fg(Color::DarkGray)), inner);
            return;
        }
    };
    let cur_pos = m.current_tape_pos().unwrap_or(tape.pos);
    let max_lines = inner.height as usize;
    let bps = app.debugger.breakpoints();

    // collect all order display rows to compute auto-scroll
    let order_entries: Vec<usize> = tape.entries.iter().enumerate()
        .filter_map(|(i, e)| if matches!(e, TapeEntry::Order(_)) { Some(i) } else { None })
        .collect();

    // find which order-row index the dis_cursor is at
    let cursor_order_row = order_entries.iter().position(|&i| i == app.dis_cursor)
        .or_else(|| order_entries.iter().position(|&i| i >= app.dis_cursor))
        .unwrap_or(0);

    // find which order-row index dis_scroll starts at
    let scroll_order_row = order_entries.iter().position(|&i| i >= app.dis_scroll).unwrap_or(0);

    // auto-scroll: if cursor is below viewport bottom, adjust scroll
    let scroll_order_row = if cursor_order_row >= scroll_order_row + max_lines {
        cursor_order_row + 1 - max_lines
    } else {
        scroll_order_row
    };
    let start_entry = order_entries.get(scroll_order_row).copied().unwrap_or(0);

    let mut lines: Vec<Line> = Vec::new();
    let mut count = 0;
    for (idx, entry) in tape.entries.iter().enumerate().skip(start_entry) {
        if count >= max_lines { break; }
        let inline = tape.inline_comments.iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, c)| c.clone());
        let at_ip = idx == cur_pos;
        let at_cursor = focused && idx == app.dis_cursor;
        if let TapeEntry::Order(o) = entry {
            let bp = bps.iter().find(|b| {
                b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == idx + 1)
            });
            let (bp_char, bp_style) = bp_marker_char(bp.map(|b| b.id), bp.map(|b| b.enabled));
            let ip_char = if at_ip { "▶" } else { " " };
            let ip_style = if at_ip {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mut spans = vec![
                Span::styled(bp_char, bp_style),
                Span::styled(ip_char, ip_style),
                Span::styled(format!("{:4}: {:05}  ", idx + 1, o), Style::default().fg(Color::DarkGray)),
                Span::raw(disassemble(*o)),
            ];
            if let Some(comment) = inline {
                spans.push(Span::styled(
                    format!("  ; {}", comment),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            let line = Line::from(spans);
            if at_cursor {
                lines.push(line.style(Style::default().add_modifier(Modifier::REVERSED)));
            } else {
                lines.push(line);
            }
            count += 1;
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled("(no orders)", Style::default().fg(Color::DarkGray))));
    }

    f.render_widget(Paragraph::new(Text::from(lines)).scroll((0, app.dis_hscroll as u16)), inner);
}

fn render_log(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Log;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Output ")
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let height = inner.height as usize;
    let total = app.log.len();
    // scroll: 0 = bottom, positive = scroll up
    let start = if app.log_scroll >= total {
        0
    } else {
        total.saturating_sub(height + app.log_scroll)
    };

    let lines: Vec<Line> = app.log.iter()
        .skip(start)
        .take(height)
        .map(|l| Line::from(l.as_str()))
        .collect();
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let (text, style) = if let Some(ref target) = app.load_prompt {
        let label = match target {
            Some(i) => format!("Load file into tape {}: {}_", i + 1, app.load_prompt_buf),
            None => format!("Load all tapes from file: {}_", app.load_prompt_buf),
        };
        (label, Style::default().fg(Color::Yellow))
    } else if app.cli_mode {
        (format!("(witch) {}_", app.cli_buf), Style::default().fg(Color::Yellow))
    } else if app.running {
        (" [Space]halt  [n]step  [b]bp  [D]dis  [Tab]focus  [R]reset  [q]quit  RUNNING".into(),
         Style::default().fg(Color::Green))
    } else {
        let halted_marker = if app.debugger.machine.halted { " [HALTED]" } else { "" };
        (format!(" [Space]run  [n]step  [b]bp  [D]dis  [S]state  [O]output  [1-7]tape  [`]cli  [q]quit{}", halted_marker),
         Style::default().fg(Color::DarkGray))
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn render_edit_popup(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let popup_width = 50u16.min(area.width.saturating_sub(4));
    let popup_height = 5u16;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    let title = match &app.edit {
        Some(EditTarget::TapeEntry { tape, idx }) => format!(" Edit tape {} line {} ", tape + 1, idx + 1),
        Some(EditTarget::Store { addr }) => format!(" Edit store {} ", addr),
        Some(EditTarget::Acc) => " Edit accumulator ".into(),
        None => " Edit ".into(),
    };

    let border_style = if matches!(app.edit_err, EditError::Bad(_)) {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Yellow)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);
    let inner = block.inner(popup_area);

    f.render_widget(Clear, popup_area);
    f.render_widget(block, popup_area);

    let err_line = match &app.edit_err {
        EditError::Bad(msg) => format!("  Error: {}", msg),
        EditError::None => "  Enter=confirm  Esc=cancel".into(),
    };
    let lines = vec![
        Line::from(format!("  > {}_", app.edit_buf)),
        Line::from(Span::styled(err_line, Style::default().fg(
            if matches!(app.edit_err, EditError::Bad(_)) { Color::Red } else { Color::DarkGray }
        ))),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ── Terminal setup/teardown ───────────────────────────────────────────────────

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<(), Box<dyn Error>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn run_tui(debugger: Debugger) -> Result<(), Box<dyn Error>> {
    // Disable ANSI colors inside TUI — ratatui applies its own styles
    colored::control::set_override(false);

    let mut terminal = setup_terminal()?;
    let mut app = App::new(debugger);

    // tape_visible defaults to loaded state
    for i in 0..7 {
        app.tape_visible[i] = app.debugger.machine.tapes[i].is_some();
    }
    // Focus first loaded tape if any, else State
    let first_tape = (0..7).find(|&i| app.debugger.machine.tapes[i].is_some());
    app.focus = first_tape.map(Focus::Tape).unwrap_or(Focus::State);
    // Init dis_cursor to current IP
    if let Some(pos) = app.debugger.machine.current_tape_pos() {
        app.dis_cursor = pos;
    }

    let result = event_loop(&mut terminal, &mut app);
    restore_terminal(terminal)?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<(), Box<dyn Error>> {
    loop {
        // Auto-scroll tape cursors to IP if machine advanced
        sync_tape_cursors(app);

        terminal.draw(|f| render(f, app))?;

        if app.running && !app.debugger.machine.halted {
            for _ in 0..STEPS_PER_TICK {
                if event::poll(Duration::ZERO)? { break; }
                app.step_once();
                if !app.running || app.debugger.machine.halted { break; }
            }
            if event::poll(Duration::ZERO)? {
                if app.handle_event(event::read()?) {
                    break;
                }
            }
        } else {
            if app.handle_event(event::read()?) {
                break;
            }
        }
    }
    Ok(())
}

fn sync_tape_cursors(app: &mut App) {
    let active_tape = app.debugger.machine.active_tape_num();
    let ip_pos = app.debugger.machine.current_tape_pos();
    if let (Some(tape_num), Some(pos)) = (active_tape, ip_pos) {
        let tape_idx = tape_num - 1;
        if tape_idx < 7 {
            if let Some(tape) = app.debugger.machine.tapes[tape_idx].as_ref() {
                let comment_count_before = tape.comments.iter()
                    .filter(|(before, _)| *before <= pos)
                    .count();
                let display_row = pos + comment_count_before;
                if app.running {
                    app.tape_cursor[tape_idx] = display_row;
                    // also follow IP in dis pane when running
                    app.dis_cursor = pos;
                    if pos < app.dis_scroll {
                        app.dis_scroll = pos;
                    }
                }
            }
        }
    }
}
