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
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;

use crate::debugger::{BpKind, Breakpoint, Debugger};
use crate::machine::{Machine, Output, IP};
use crate::tape::{order_line_num, TapeEntry, WitchAcc, WitchNum};
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
    Ref,
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
    tape_hscroll: [usize; 7],
    state_cursor_row: usize, // 0=acc, 1=sign, 2-10=store decade rows 1-9
    state_cursor_col: usize, // 0-9, meaningful only for store rows
    log_hscroll: usize,
    dis_scroll: usize,
    dis_hscroll: usize,
    dis_cursor: usize,
    log_scroll: usize,
    tape_visible: [bool; 7],
    show_dis: bool,
    show_state: bool,
    show_output: bool,
    show_help: bool,
    show_ref: bool,
    ref_page: usize,
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
    // active completion cycling (Tab/Shift+Tab in load prompt)
    load_compl: Option<LoadCompl>,
    // set to trigger fzf selection in the event loop (where terminal access is available)
    fzf_request: Option<Option<usize>>,
}

struct LoadCompl {
    candidates: Vec<String>, // just filenames
    dir: String,             // path prefix including trailing slash (or "" for cwd)
    idx: usize,
}

impl App {
    fn new(debugger: Debugger) -> Self {
        App {
            debugger,
            focus: Focus::Tape(0),
            tape_cursor: [0; 7],
            tape_scroll: [0; 7],
            tape_hscroll: [0; 7],
            state_cursor_row: 2, // start on first store row
            state_cursor_col: 0,
            log_hscroll: 0,
            dis_scroll: 0,
            dis_hscroll: 0,
            dis_cursor: 0,
            log_scroll: 0,
            show_dis: true,
            show_state: true,
            show_output: true,
            show_help: false,
            show_ref: false,
            ref_page: 0,
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
            load_compl: None,
            fzf_request: None,
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
        if let Some(pos) = self.debugger.machine.current_tape_pos() {
            self.dis_cursor = pos;
            if pos < self.dis_scroll {
                self.dis_scroll = pos;
            }
            // Follow IP in tape view; render centers if off-screen
            if let Some(tape_num) = self.debugger.machine.active_tape_num() {
                let tape_idx = tape_num - 1;
                if tape_idx < 7
                    && let Some(tape) = self.debugger.machine.tapes[tape_idx].as_ref() {
                        let comment_count = tape.comments.iter().filter(|(b, _)| *b <= pos).count();
                        let block_count = tape.entries[..pos].iter().filter(|e| matches!(e, TapeEntry::Block(_))).count();
                        self.tape_cursor[tape_idx] = (pos - block_count) + comment_count;
                    }
            }
        }
    }

    fn visible_tapes(&self) -> Vec<usize> {
        (0..7).filter(|&i| self.tape_visible[i]).collect()
    }

    fn focus_panes(&self) -> Vec<Focus> {
        let visible = self.visible_tapes();
        let mut panes: Vec<Focus> = visible.iter().map(|&i| Focus::Tape(i)).collect();
        if self.show_state  { panes.push(Focus::State); }
        if self.show_dis    { panes.push(Focus::Dis); }
        if self.show_output { panes.push(Focus::Log); }
        if self.show_ref    { panes.push(Focus::Ref); }
        panes
    }

    fn cycle_focus(&mut self) {
        let panes = self.focus_panes();
        if panes.is_empty() { return; }
        let cur = panes.iter().position(|p| p == &self.focus).unwrap_or(0);
        self.focus = panes[(cur + 1) % panes.len()];
    }

    fn cycle_focus_backward(&mut self) {
        let panes = self.focus_panes();
        if panes.is_empty() { return; }
        let cur = panes.iter().position(|p| p == &self.focus).unwrap_or(0);
        self.focus = panes[(cur + panes.len() - 1) % panes.len()];
    }

    fn scroll_left(&mut self) {
        match self.focus {
            Focus::Tape(i) => self.tape_hscroll[i] = self.tape_hscroll[i].saturating_sub(4),
            Focus::State => {
                if self.state_cursor_row >= 2 {
                    self.state_cursor_col = self.state_cursor_col.saturating_sub(1);
                }
            }
            Focus::Dis => self.dis_hscroll = self.dis_hscroll.saturating_sub(4),
            Focus::Log => self.log_hscroll = self.log_hscroll.saturating_sub(4),
            Focus::Ref => {}
        }
    }

    fn scroll_right(&mut self) {
        match self.focus {
            Focus::Tape(i) => self.tape_hscroll[i] += 4,
            Focus::State => {
                if self.state_cursor_row >= 2 {
                    self.state_cursor_col = (self.state_cursor_col + 1).min(9);
                }
            }
            Focus::Dis => self.dis_hscroll += 4,
            Focus::Log => self.log_hscroll += 4,
            Focus::Ref => {}
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
            Focus::State => {
                if self.state_cursor_row > 0 { self.state_cursor_row -= 1; }
            }
            Focus::Dis => self.move_dis_cursor(-1),
            Focus::Log => {
                let max = self.log.len().saturating_sub(1);
                self.log_scroll = (self.log_scroll + 1).min(max);
            }
            Focus::Ref => {
                self.ref_page = self.ref_page.saturating_sub(1);
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
            Focus::State => {
                if self.state_cursor_row < 10 { self.state_cursor_row += 1; }
            }
            Focus::Dis => self.move_dis_cursor(1),
            Focus::Log => {
                if self.log_scroll > 0 {
                    self.log_scroll -= 1;
                }
            }
            Focus::Ref => {
                self.ref_page = (self.ref_page + 1).min(REF_PAGE_COUNT - 1);
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
        self.debugger.breakpoints().iter().find(|b| {
            b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == entry_idx)
        })
    }

    fn toggle_bp(&mut self) {
        // Block marker: set BpKind::Block breakpoint (fires at first order after the marker)
        if let Focus::Tape(i) = self.focus {
            let cursor = self.tape_cursor[i];
            if let Some(entry_idx) = display_row_to_entry(&self.debugger.machine, i, cursor) {
                let tape_num = i + 1;
                let block_num = self.debugger.machine.tapes[i].as_ref()
                    .and_then(|t| t.entries.get(entry_idx))
                    .and_then(|e| if let TapeEntry::Block(b) = e { Some(*b) } else { None });
                if let Some(b_val) = block_num {
                    let existing = self.debugger.breakpoints().iter()
                        .find(|bp| bp.tape == tape_num && matches!(bp.kind, BpKind::Block(blk) if blk == b_val))
                        .map(|bp| bp.id);
                    if let Some(id) = existing {
                        let outs = self.debugger.execute(&format!("break rm {}", id));
                        for l in outs { self.push_log(l); }
                    } else {
                        let outs = self.debugger.execute(&format!("break block {} {}", b_val, tape_num));
                        for l in outs { self.push_log(l); }
                    }
                    return;
                }
            }
        }
        let (tape_num, entry_idx) = match self.focus {
            Focus::Tape(i) => {
                let cursor = self.tape_cursor[i];
                let entry_idx = match display_row_to_entry(&self.debugger.machine, i, cursor) {
                    Some(idx) => idx,
                    None => return,
                };
                (i + 1, entry_idx)
            }
            Focus::Dis => {
                let tape_num = match self.debugger.machine.active_tape_num() {
                    Some(n) => n,
                    None => return,
                };
                (tape_num, self.dis_cursor)
            }
            _ => return,
        };
        // check if bp already exists (BpKind::Line stores raw entry_idx)
        let existing = self.debugger.breakpoints().iter().find(|b| {
            b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == entry_idx)
        }).map(|b| b.id);
        if let Some(id) = existing {
            let outs = self.debugger.execute(&format!("break rm {}", id));
            for l in outs { self.push_log(l); }
        } else {
            // break line command takes order line num (skipping blocks)
            let lnum = self.debugger.machine.tapes.get(tape_num - 1).and_then(|t| t.as_ref())
                .map(|t| order_line_num(&t.entries, entry_idx)).unwrap_or(entry_idx + 1);
            let outs = self.debugger.execute(&format!("break line {} {}", lnum, tape_num));
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
        if let Some(Some(tape)) = self.debugger.machine.tapes.get_mut(tape_idx)
            && entry_idx < tape.entries.len() {
                tape.entries.remove(entry_idx);
                if tape.pos > entry_idx && tape.pos > 0 {
                    tape.pos -= 1;
                }
                // sync IP if this tape is active
                if let IP::Tape { reader, ref mut pos } = self.debugger.machine.ip
                    && reader == tape_idx {
                        *pos = self.debugger.machine.tapes[tape_idx]
                            .as_ref().map(|t| t.pos).unwrap_or(0);
                    }
                if self.tape_cursor[tape_idx] > 0 {
                    self.tape_cursor[tape_idx] -= 1;
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
                        if let Some(Some(t)) = self.debugger.machine.tapes.get_mut(tape)
                            && idx < t.entries.len() {
                                t.entries[idx] = entry;
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
            KeyCode::Char('?') => { self.show_help = !self.show_help; }
            _ if self.show_help => { self.show_help = false; }
            KeyCode::Char(' ') => {
                if self.debugger.machine.halted {
                    self.push_log("halted — use r to reset".to_string());
                } else {
                    self.running = !self.running;
                }
            }
            KeyCode::Char('n') => {
                self.running = false;
                self.do_step_cmd();
            }
            KeyCode::Char('r') => {
                let outs = self.debugger.execute("reset");
                for l in outs { self.push_log(l); }
                self.running = false;
            }
            KeyCode::Char('R') => {
                self.show_ref = !self.show_ref;
                if !self.show_ref && self.focus == Focus::Ref {
                    self.cycle_focus();
                }
            }
            KeyCode::Char('D') => self.show_dis = !self.show_dis,
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::BackTab => self.cycle_focus_backward(),
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
                Focus::State => { self.state_cursor_row = 0; self.state_cursor_col = 0; }
                Focus::Dis => self.dis_scroll = 0,
                Focus::Log => self.log_scroll = self.log.len().saturating_sub(1),
                Focus::Ref => self.ref_page = 0,
            },
            KeyCode::Char('G') => match self.focus {
                Focus::Tape(i) => {
                    let len = tape_display_len(&self.debugger.machine, i);
                    self.tape_cursor[i] = len.saturating_sub(1);
                }
                Focus::State => { self.state_cursor_row = 10; self.state_cursor_col = 9; }
                Focus::Dis => self.dis_scroll = 100,
                Focus::Log => self.log_scroll = 0,
                Focus::Ref => self.ref_page = REF_PAGE_COUNT - 1,
            },
            KeyCode::Enter => {
                match self.focus {
                    Focus::Tape(i) => self.begin_edit_tape(i),
                    Focus::State => {
                        match self.state_cursor_row {
                            0 => self.begin_edit_store(8),
                            1 => {
                                self.debugger.machine.sign_flag = match self.debugger.machine.sign_flag {
                                    None => Some(true),
                                    Some(true) => Some(false),
                                    Some(false) => None,
                                };
                            }
                            r => self.begin_edit_store((r - 1) * 10 + self.state_cursor_col),
                        }
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
            KeyCode::Char('f') => {
                let tape_idx = match self.focus {
                    Focus::Tape(i) => i,
                    _ => self.visible_tapes().first().copied().unwrap_or(0),
                };
                self.fzf_request = Some(Some(tape_idx));
            }
            KeyCode::Char('F') => {
                self.fzf_request = Some(None);
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
                self.load_compl = None;
            }
            KeyCode::Tab | KeyCode::Char('\t') => {
                self.compl_step(1);
            }
            KeyCode::BackTab => {
                self.compl_step(-1);
            }
            KeyCode::Backspace => {
                self.load_compl = None;
                self.load_prompt_buf.pop();
            }
            KeyCode::Enter => {
                let path = self.load_prompt_buf.trim().to_string();
                let target = self.load_prompt.take();
                self.load_prompt_buf.clear();
                if !path.is_empty()
                    && let Some(t) = target { self.load_path(&path, t); }
            }
            KeyCode::Char(c) => {
                self.load_compl = None;
                self.load_prompt_buf.push(c);
            }
            _ => {}
        }
        false
    }

    fn load_path(&mut self, path: &str, target: Option<usize>) {
        match target {
            Some(tape_idx) => {
                let outs = self.debugger.execute(&format!("load {} {}", path, tape_idx + 1));
                for l in outs { self.push_log(l); }
                if self.debugger.machine.tapes[tape_idx].is_some() {
                    self.tape_visible[tape_idx] = true;
                }
            }
            None => {
                let outs = self.debugger.execute(&format!("load {}", path));
                for l in outs { self.push_log(l); }
                for i in 0..7 {
                    if self.debugger.machine.tapes[i].is_some() { self.tape_visible[i] = true; }
                }
            }
        }
    }

    fn compl_step(&mut self, delta: i32) {
        use std::path::Path;
        if let Some(ref mut c) = self.load_compl {
            // already cycling — advance
            let n = c.candidates.len();
            c.idx = ((c.idx as i64 + delta as i64).rem_euclid(n as i64)) as usize;
            let name = &c.candidates[c.idx];
            let path = format!("{}{}", c.dir, name);
            self.load_prompt_buf = if Path::new(&path).is_dir() {
                format!("{}/", path)
            } else {
                path
            };
        } else {
            // first tab — run completion
            let (completed, candidates, dir) = tab_complete_path(&self.load_prompt_buf);
            if let Some(new_buf) = completed {
                // unique or common-prefix advance: apply and don't enter cycling
                self.load_prompt_buf = new_buf;
            } else if !candidates.is_empty() {
                // ambiguous: enter cycling, start at idx 0 (or last if backward)
                let idx = if delta < 0 { candidates.len() - 1 } else { 0 };
                let name = &candidates[idx];
                let path = format!("{}{}", dir, name);
                self.load_prompt_buf = if Path::new(&path).is_dir() {
                    format!("{}/", path)
                } else {
                    path
                };
                self.load_compl = Some(LoadCompl { candidates, dir, idx });
            }
        }
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
        let non_block = t.entries.iter().filter(|e| !matches!(e, TapeEntry::Block(_))).count();
        non_block + t.comments.len()
    }).unwrap_or(0)
}

/// Map a display row (cursor) to an entry index, skipping comment rows.
fn display_row_to_entry(machine: &Machine, tape_idx: usize, display_row: usize) -> Option<usize> {
    let tape = machine.tapes[tape_idx].as_ref()?;
    let mut row = 0usize;
    let mut comment_idx = 0usize;
    for (entry_idx, entry) in tape.entries.iter().enumerate() {
        while comment_idx < tape.comments.len() && tape.comments[comment_idx].0 == entry_idx {
            if row == display_row {
                return None; // cursor is on a comment row
            }
            row += 1;
            comment_idx += 1;
        }
        // Block entries are folded into the next entry's gutter, not a separate display row
        if matches!(entry, TapeEntry::Block(_)) {
            continue;
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
    let mut pending_block: Option<(u8, Option<usize>, Option<bool>)> = None;
    for (entry_idx, entry) in tape.entries.iter().enumerate() {
        while comment_idx < tape.comments.len() && tape.comments[comment_idx].0 == entry_idx {
            rows.push(TapeRow::Comment(tape.comments[comment_idx].1.clone()));
            comment_idx += 1;
        }
        if let TapeEntry::Block(b_val) = entry {
            let bp = bps.iter().find(|bp| {
                bp.tape == tape_num && matches!(bp.kind, BpKind::Block(blk) if blk == *b_val)
            });
            pending_block = Some((*b_val, bp.map(|b| b.id), bp.map(|b| b.enabled)));
            continue;
        }
        let at_cur = ip_pos == Some(entry_idx);
        let bp = bps.iter().find(|b| {
            b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == entry_idx)
        });
        let inline = tape.inline_comments.iter()
            .find(|(i, _)| *i == entry_idx)
            .map(|(_, c)| c.clone());
        let (block_num, block_bp_id, block_bp_enabled) = match pending_block.take() {
            Some((n, id, en)) => (Some(n), id, en),
            None => (None, None, None),
        };
        let line_num = order_line_num(&tape.entries, entry_idx);
        rows.push(TapeRow::Entry { line_num, entry: entry.clone(), at_cur, bp_id: bp.map(|b| b.id), bp_enabled: bp.map(|b| b.enabled), inline, block_num, block_bp_id, block_bp_enabled });
    }
    rows
}

enum TapeRow {
    Comment(String),
    Entry {
        line_num: usize,
        entry: TapeEntry,
        at_cur: bool,
        bp_id: Option<usize>,
        bp_enabled: Option<bool>,
        inline: Option<String>,
        block_num: Option<u8>,
        block_bp_id: Option<usize>,
        block_bp_enabled: Option<bool>,
    },
}

/// Returns (completed_buf, candidates, dir_prefix).
/// completed_buf = Some when there is a unique or common-prefix completion.
/// candidates = all matching filenames (for cycling).
/// dir_prefix = the "dir/" string to prepend to candidates when cycling.
fn tab_complete_path(buf: &str) -> (Option<String>, Vec<String>, String) {
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
    let Ok(rd) = std::fs::read_dir(dir_search) else { return (None, vec![], String::new()); };
    let mut matches: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with(file_prefix) { Some(name) } else { None }
        })
        .collect();
    matches.sort();

    // build the dir prefix used when constructing full paths for cycling
    let dir_prefix = if dir_part == "." && !buf.contains('/') {
        String::new()
    } else {
        format!("{}/", dir_part.trim_end_matches('/'))
    };

    if matches.is_empty() { return (None, vec![], dir_prefix); }

    // common prefix of all matches
    let first = &matches[0];
    let mut len = first.len();
    for s in &matches[1..] {
        len = len.min(s.len());
        for (i, (a, b)) in first.bytes().zip(s.bytes()).enumerate() {
            if a != b { len = len.min(i); break; }
        }
    }

    if len <= file_prefix.len() {
        // no prefix progress — return candidates for cycling
        return (None, matches, dir_prefix);
    }

    let completed_name = first[..len].to_string();
    let new_path = format!("{}{}", dir_prefix, completed_name);
    let new_path = if matches.len() == 1 && Path::new(&new_path).is_dir() {
        format!("{}/", new_path)
    } else {
        new_path
    };
    (Some(new_path), matches, dir_prefix)
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

    // Split horizontally when ref panel is visible
    let (left_area, ref_area_opt) = if app.show_ref {
        let hchunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(72)])
            .split(main_area);
        (hchunks[0], Some(hchunks[1]))
    } else {
        (main_area, None)
    };

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
        if let Some(ra) = ref_area_opt { render_ref_panel(f, app, ra); }
        render_status(f, app, status_area);
        return;
    }
    // Last row fills remaining space
    if let Some(last) = v_constraints.last_mut() { *last = Constraint::Min(3); }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(left_area);

    // Tape row
    if let Some(ri) = row_tape {
        let n_cols = visible.len().clamp(1, 4);
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

    // Ref panel (right column)
    if let Some(ra) = ref_area_opt { render_ref_panel(f, app, ra); }

    render_status(f, app, status_area);

    if app.edit.is_some() { render_edit_popup(f, app, area); }
    if app.show_help { render_help_popup(f, area); }
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

    // Auto-scroll: center on cursor if it goes off-screen
    let scroll = {
        let mut s = scroll;
        if cursor < s || cursor >= s + visible_height {
            s = cursor.saturating_sub(visible_height / 2);
        }
        s
    };

    let hscroll = app.tape_hscroll[tape_idx];
    let mut max_content_width = 0usize;

    let lines: Vec<Line> = rows.iter().enumerate().skip(scroll).take(visible_height)
        .map(|(row_idx, row)| {
            let selected = focused && row_idx == cursor;
            match row {
                TapeRow::Comment(text) => {
                    let s = format!("  ; {}", text);
                    max_content_width = max_content_width.max(s.chars().count());
                    let line = Line::from(Span::styled(s, Style::default().fg(Color::DarkGray)));
                    if selected { line.style(Style::default().add_modifier(Modifier::REVERSED)) } else { line }
                }
                TapeRow::Entry { line_num, entry, at_cur, bp_id, bp_enabled, inline, block_num, block_bp_id, block_bp_enabled } => {
                    let (bp_char, bp_style) = bp_marker_char(*bp_id, *bp_enabled);
                    let cur_char = if *at_cur { "▶" } else { " " };
                    let cur_style = if *at_cur {
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    let num_style = Style::default().fg(Color::DarkGray);
                    let entry_text = entry.to_string();
                    let num_str = format!("{:4}: ", line_num);
                    // 4-char block gutter + 2-char bp/cur gutter + num_str + text
                    let mut width = 4 + 1 + 1 + num_str.chars().count() + entry_text.chars().count();
                    let mut spans: Vec<Span> = Vec::new();
                    if let Some(blk) = block_num {
                        let (blk_bp_char, blk_bp_style) = bp_marker_char(*block_bp_id, *block_bp_enabled);
                        spans.push(Span::styled(blk_bp_char, blk_bp_style));
                        spans.push(Span::styled(format!("[{}]", blk), Style::default().fg(Color::Magenta)));
                    } else {
                        spans.push(Span::raw("    "));
                    }
                    spans.extend([
                        Span::styled(bp_char, bp_style),
                        Span::styled(cur_char, cur_style),
                        Span::styled(num_str, num_style),
                        Span::raw(entry_text),
                    ]);
                    if let Some(comment) = inline {
                        let cs = format!("  ; {}", comment);
                        width += cs.chars().count();
                        spans.push(Span::styled(cs, Style::default().fg(Color::DarkGray)));
                    }
                    max_content_width = max_content_width.max(width);
                    let line = Line::from(spans);
                    if selected { line.style(Style::default().add_modifier(Modifier::REVERSED)) } else { line }
                }
            }
        })
        .collect();

    let eff_hscroll = hscroll.min(max_content_width.saturating_sub(inner.width as usize));
    let p = Paragraph::new(Text::from(lines)).scroll((0, eff_hscroll as u16));
    f.render_widget(p, inner);
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

    let sel_val_style = |base: Style| if focused { base.add_modifier(Modifier::REVERSED) } else { base };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("acc:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(m.acc.to_string(),
                if focused && app.state_cursor_row == 0 { Style::default().add_modifier(Modifier::REVERSED) }
                else { Style::default() }),
        ]),
        Line::from(vec![
            Span::styled("sign: ", Style::default().fg(Color::DarkGray)),
            Span::styled(sign_str,
                if focused && app.state_cursor_row == 1 { Style::default().add_modifier(Modifier::REVERSED) }
                else { Style::default() }),
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
        Line::from(Span::styled(
            format!("    {}", (0..10).map(|c| format!("{:>12}", c)).collect::<Vec<_>>().join(" ")),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    // Store grid: one row per tens-decade (10-19, 20-29, ..., 90-99)
    // cursor_row 2 = decade 1, ..., cursor_row 10 = decade 9
    for decade in 1..=9usize {
        let base = decade * 10;
        let cursor_on_row = focused && app.state_cursor_row == decade + 1;
        let mut spans = vec![Span::styled(
            format!("{:2}: ", base),
            Style::default().fg(Color::DarkGray),
        )];
        for col in 0..10usize {
            let addr = base + col;
            if addr > 99 { break; }
            let val = m.stores[addr - 10];
            let base_style = if val.negative && val.magnitude > 0 {
                Style::default().fg(Color::Red)
            } else if val.magnitude > 0 {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let style = if cursor_on_row && app.state_cursor_col == col {
                sel_val_style(base_style)
            } else {
                base_style
            };
            spans.push(Span::styled(format!("{:>12}", val.to_string()), style));
            if col < 9 { spans.push(Span::raw(" ")); }
        }
        lines.push(Line::from(spans));
    }

    // Split: fixed top (acc/sign/ip/halt/divider, 5 lines, no scroll) +
    //        scrollable body (col-header + 9 decades, h-scroll + v-scroll)
    let body_lines = lines.split_off(5);
    let head_lines = lines;

    let head_h = 5usize.min(inner.height as usize);
    let body_h = (inner.height as usize).saturating_sub(head_h);

    // Vertical scroll: col-header is body line 0, decade N is body line N+1 (cursor_row 2..=10)
    let body_v = if app.state_cursor_row >= 2 && body_h > 0 {
        let line = app.state_cursor_row - 1; // header=0, decade1=1, ..., decade9=9
        if line + 1 > body_h { (line + 1 - body_h) as u16 } else { 0 }
    } else { 0 };

    // Horizontal scroll: column n occupies [4+n*13, 15+n*13] in each row
    let body_hscroll = if app.state_cursor_row >= 2 {
        let col_end = 15 + app.state_cursor_col * 13;
        let w = inner.width as usize;
        if col_end + 2 > w { (col_end + 2 - w) as u16 } else { 0 }
    } else { 0 };

    if head_h > 0 {
        let hr = Rect::new(inner.x, inner.y, inner.width, head_h as u16);
        f.render_widget(Paragraph::new(Text::from(head_lines)), hr);
    }
    if body_h > 0 {
        let br = Rect::new(inner.x, inner.y + head_h as u16, inner.width, body_h as u16);
        f.render_widget(
            Paragraph::new(Text::from(body_lines)).scroll((body_v, body_hscroll)),
            br,
        );
    }
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

    // auto-scroll: center on cursor if it goes off-screen
    let scroll_order_row = if cursor_order_row < scroll_order_row || cursor_order_row >= scroll_order_row + max_lines {
        cursor_order_row.saturating_sub(max_lines / 2)
    } else {
        scroll_order_row
    };
    let start_entry = order_entries.get(scroll_order_row).copied().unwrap_or(0);

    // scan backward from start_entry to find a block marker that belongs to the first visible order
    let mut pre_block: Option<u8> = None;
    for e in tape.entries[..start_entry].iter().rev() {
        match e {
            TapeEntry::Block(b) => { pre_block = Some(*b); break; }
            TapeEntry::Order(_) => break,
            _ => {}
        }
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut pending_block: Option<(u8, Option<usize>, Option<bool>)> = if let Some(b) = pre_block {
        let bp = bps.iter().find(|bp| bp.tape == tape_num && matches!(bp.kind, BpKind::Block(blk) if blk == b));
        Some((b, bp.map(|b| b.id), bp.map(|b| b.enabled)))
    } else { None };
    for (idx, entry) in tape.entries.iter().enumerate().skip(start_entry) {
        if lines.len() >= max_lines { break; }
        match entry {
            TapeEntry::Block(b) => {
                let bp = bps.iter().find(|bp| {
                    bp.tape == tape_num && matches!(bp.kind, BpKind::Block(blk) if blk == *b)
                });
                pending_block = Some((*b, bp.map(|b| b.id), bp.map(|b| b.enabled)));
            }
            TapeEntry::Order(o) => {
                let inline = tape.inline_comments.iter()
                    .find(|(i, _)| *i == idx)
                    .map(|(_, c)| c.clone());
                let at_ip = idx == cur_pos;
                let at_cursor = focused && idx == app.dis_cursor;
                let bp = bps.iter().find(|b| {
                    b.tape == tape_num && matches!(b.kind, BpKind::Line(n) if n == idx)
                });
                let (bp_char, bp_style) = bp_marker_char(bp.map(|b| b.id), bp.map(|b| b.enabled));
                let ip_char = if at_ip { "▶" } else { " " };
                let ip_style = if at_ip {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let lnum = order_line_num(&tape.entries, idx);
                let mut spans: Vec<Span> = Vec::new();
                if let Some((blk, blk_bp_id, blk_bp_enabled)) = pending_block.take() {
                    let (blk_bp_char, blk_bp_style) = bp_marker_char(blk_bp_id, blk_bp_enabled);
                    spans.push(Span::styled(blk_bp_char, blk_bp_style));
                    spans.push(Span::styled(format!("[{}]", blk), Style::default().fg(Color::Magenta)));
                } else {
                    spans.push(Span::raw("    "));
                }
                spans.extend([
                    Span::styled(bp_char, bp_style),
                    Span::styled(ip_char, ip_style),
                    Span::styled(format!("{:4}: {:05}  ", lnum, o), Style::default().fg(Color::DarkGray)),
                    Span::raw(disassemble(*o)),
                ]);
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
            }
            _ => {}
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled("(no orders)", Style::default().fg(Color::DarkGray))));
    }

    let max_width: usize = lines.iter()
        .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0);
    let eff_hscroll = app.dis_hscroll.min(max_width.saturating_sub(inner.width as usize));
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((0, eff_hscroll as u16)), inner);
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
    let max_width: usize = lines.iter()
        .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0);
    let eff_hscroll = app.log_hscroll.min(max_width.saturating_sub(inner.width as usize));
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((0, eff_hscroll as u16)), inner);
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
        (" [Space]halt  [n]step  [b]bp  [D]dis  [Tab]focus  [r]reset  [R]ref  [q]quit  RUNNING".into(),
         Style::default().fg(Color::Green))
    } else {
        let halted_marker = if app.debugger.machine.halted { " [HALTED]" } else { "" };
        (format!(" [Space]run  [n]step  [b]bp  [D]dis  [S]state  [O]output  [R]ref  [1-7]tape  [`]cli  [q]quit{}", halted_marker),
         Style::default().fg(Color::DarkGray))
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn render_help_popup(f: &mut ratatui::Frame, area: Rect) {
    let lines: &[(&str, &str)] = &[
        ("Navigation", ""),
        ("Tab / Shift+Tab", "cycle focus"),
        ("j/k  ↑/↓",        "scroll up/down"),
        ("h/l  ←/→",        "scroll left/right (h/l in state/dis)"),
        ("g / G",            "jump to top / bottom"),
        ("", ""),
        ("Execution", ""),
        ("Space",            "run / pause"),
        ("n",                "step one order"),
        ("r",                "reset (rewind tape 1, seek block 1)"),
        ("b",                "toggle breakpoint at cursor"),
        ("Ctrl+C",           "stop running"),
        ("", ""),
        ("Tape editing", ""),
        ("Enter",            "edit value at cursor"),
        ("i / a",            "insert entry before / after cursor"),
        ("x / Del",          "delete entry at cursor"),
        ("", ""),
        ("Load / unload", ""),
        ("f",                "load tape into focused tape pane (fzf)"),
        ("F",                "load all tapes from directory (fzf)"),
        ("u / U",            "unload focused tape / all tapes"),
        ("", ""),
        ("Pane toggles", ""),
        ("1–7",              "show/hide tape pane"),
        ("S",                "show/hide state pane"),
        ("D",                "show/hide disassembly pane"),
        ("O",                "show/hide output pane"),
        ("R",                "show/hide reference panel (j/k to page)"),
        ("", ""),
        ("Other", ""),
        ("`",                "open CLI command prompt"),
        ("q",                "quit"),
        ("?",                "close this help"),
    ];

    let content_w = 64u16;
    let content_h = lines.len() as u16 + 2;
    let popup_w = content_w.min(area.width.saturating_sub(4));
    let popup_h = content_h.min(area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(popup_w) / 2;
    let y = area.y + area.height.saturating_sub(popup_h) / 2;
    let popup_area = Rect::new(x, y, popup_w, popup_h);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup_area);

    let rendered: Vec<Line> = lines.iter().map(|(key, desc)| {
        if key.is_empty() && desc.is_empty() {
            Line::from("")
        } else if desc.is_empty() {
            // section header
            Line::from(Span::styled(key.to_string(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
        } else {
            Line::from(vec![
                Span::styled(format!("  {:20}", key), Style::default().fg(Color::Cyan)),
                Span::raw(desc.to_string()),
            ])
        }
    }).collect();

    f.render_widget(Clear, popup_area);
    f.render_widget(block, popup_area);
    f.render_widget(Paragraph::new(Text::from(rendered)), inner);
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

// ── Reference panel ───────────────────────────────────────────────────────────

const REF_PAGE_COUNT: usize = 6;

fn ref_page_lines(page: usize) -> Vec<Line<'static>> {
    // h = section header, r = table row (key | value), t = plain text, b = blank
    macro_rules! h { ($s:expr) => { Line::from(Span::styled($s, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))) }; }
    macro_rules! r { ($k:expr, $v:expr) => { Line::from(vec![
        Span::styled(format!("  {:18}", $k), Style::default().fg(Color::Cyan)),
        Span::raw($v),
    ]) }; }
    macro_rules! t { ($s:expr) => { Line::from(Span::styled(format!("  {}", $s), Style::default().fg(Color::Gray))) }; }
    macro_rules! b { () => { Line::from("") }; }

    match page {
        0 => vec![
            h!("Arithmetic  (ops 1–7)"),
            b!(),
            r!("1 ss rr", "Add, hold       dest += src"),
            r!("2 ss rr", "Add, clear      dest += src; src = 0"),
            r!("3 ss rr", "Subtract, hold  dest -= src"),
            r!("4 ss rr", "Sub, clear      dest -= src; src = 0"),
            r!("5 ss rr", "Multiply        acc += src×dest; dest = 0"),
            r!("6 ss rr", "Divide          dest = acc/src; acc = rem"),
            r!("7 ss rr", "Modulus, hold   dest += |src|"),
            b!(),
            h!("Arithmetic Constraints"),
            b!(),
            t!("Ops 2/4 need shift==2 (default); not on exc. pairs"),
            b!(),
            t!("Ops 5/6: src and dest must NOT be in 00–09"),
            b!(),
            t!("|result| ≥ 10 → alarm stop"),
            b!(),
            t!("Neg multiplier can transiently overflow even if"),
            t!("  final product fits — machine alarm may fire"),
            b!(),
            t!("Divide: oversized quotient runs long (no cap)"),
            t!("Exact divide: +dividend off by -1 in last digit"),
            b!(),
            h!("Shift (08n00) — applies to next 1/3/7 only"),
            b!(),
            r!("A  ×10", "B  ×1  C  ×10⁻¹  D  ×10⁻²"),
            r!("E  ×10⁻³", "F  ×10⁻⁴  G  ×10⁻⁵  H  ×10⁻⁶"),
            r!("J  ×10⁻⁷", "(shift letter in digit 3 of 08n00)"),
            t!("A drops leading digit. C–J drop trailing digits"),
            t!("  (into acc extra low digits, not lost)"),
            t!("shift not consumed by I/O, mul, or div orders"),
        ],
        1 => vec![
            h!("Control  (ops 00x)"),
            b!(),
            r!("00000", "No-op — ignored"),
            r!("00100", "Finish — if held: continue; else lamp+alarm"),
            r!("00200", "Signal — if held: continue; else lamp+alarm"),
            b!(),
            h!("Sign Test"),
            b!(),
            r!("011 dd", "flag = (dd is positive)"),
            r!("012 dd", "flag = (dd is negative)"),
            b!(),
            h!("Transfer Control"),
            b!(),
            r!("021 rr", "Jump to rr (reader or store location)"),
            r!("022 rr", "Cond jump — flag unset→STOP, true→jump,"),
            t!("            false→next order"),
            b!(),
            h!("Search"),
            b!(),
            r!("03 b rr", "Search tape rr for block b"),
            r!("05 b rr", "Cond search — flag true→search, else next"),
            t!("b not on tape → hangs. Alarm if no separator"),
            t!("  in ~30s, or separator persists >30s"),
            b!(),
            h!("Output Control"),
            b!(),
            r!("07 n", "Set output layout (see p.4)"),
            r!("08 n 00", "Set shift factor (see p.1)"),
        ],
        2 => vec![
            h!("Order Format"),
            b!(),
            t!("Assrr  — A=opcode, ss=source, rr=dest"),
            t!("Control orders start with 0 (5 digits)"),
            t!("All values sign-magnitude, 8 decimal digits"),
            t!("+0 and -0 are distinct"),
            b!(),
            h!("Arithmetic Registers"),
            b!(),
            r!("10–19", "group 1  (decade 1)"),
            r!("20–29", "group 2  (decade 2)"),
            r!("30–39", "group 3  (decade 3)"),
            r!("...     90–99", "groups 4–9"),
            r!("09", "accumulator (whole, ±16 digits)"),
            r!("08", "acc low-7 digits (×10⁻⁸ as dest)"),
            b!(),
            h!("Same-Group Rule"),
            b!(),
            t!("ss and rr must have different first digit"),
            t!("(different decade / group)"),
            t!("Exceptions for 00–09 addresses — see p.5"),
        ],
        3 => vec![
            h!("Output Layout  (07n)"),
            b!(),
            r!("0", "Feed 5 blank rows"),
            r!("1", "Punch, 8 digits"),
            r!("2", "Punch, 5 digits + *"),
            r!("3", "Print, 8 digits, 5 cols, first/mid position"),
            r!("4", "Print, 8 digits, line end"),
            r!("5", "Print, 8 digits, line end + blank line"),
            r!("6", "Print, 6 digits, 6 cols, first/mid"),
            r!("7", "Print, 6 digits, 5 cols, first/mid"),
            r!("8", "Print, 6 digits, line end"),
            r!("9", "Print, 6 digits, line end + blank line"),
            b!(),
            t!("layout must be set before any output order"),
            t!("first/mid: item goes in next column slot;"),
            t!("  line-end layouts flush the line buffer"),
            b!(),
            h!("Output Destinations"),
            b!(),
            r!("01", "Printer (write-only; set layout first)"),
            r!("02", "Perforator / punch"),
            r!("03", "Printer (second output channel)"),
            r!("04", "Perforator (second punch)"),
            b!(),
            h!("Input Sources (as ss)"),
            b!(),
            r!("01–07", "Reader tapes 1–7 (advance tape pos)"),
        ],
        4 => vec![
            h!("Special Addresses  00–09"),
            b!(),
            h!("As source (ss)"),
            b!(),
            r!("00", "Round-off: random 0 or 1, sign auto-matched"),
            r!("", "  (7th decimal digit position by default)"),
            r!("01–07", "Reader tapes 1–7 (read next value)"),
            r!("08", "Acc low 7 digits + acc sign  (×10⁸ true val)"),
            r!("09", "Whole accumulator  (±16 digits)"),
            b!(),
            h!("As destination (rr)"),
            b!(),
            r!("00", "Drain (discard written value)"),
            r!("01", "Printer  (layout required)"),
            r!("02", "Perforator"),
            r!("03", "Printer  (layout required)"),
            r!("04", "Perforator"),
            r!("05–07", "Spare (no effect)"),
            r!("08", "Acc low 7 digits, scaled ×10⁻⁸ (8th dropped)"),
            r!("09", "Whole accumulator"),
            b!(),
            h!("00–09 Exception Pairs  (no op 2/4)"),
            b!(),
            t!("00→09  01-07→00  01-07→09  08→00"),
            t!("08→01-04  09→00  09→01-04"),
            t!("08 is only path to acc's lowest 7 digits"),
            t!("  (beats any shift; max ×10⁻⁷ not enough)"),
        ],
        5 => vec![
            h!("Tape & Block Structure"),
            b!(),
            t!("Tape: sequence of blocks separated by markers"),
            t!("Block: header 'b' (1 digit) then data entries"),
            t!("Entries: 8-digit sign-magnitude numbers"),
            t!("Orders: 5-digit codes (Assrr) stored as values"),
            b!(),
            h!("Block Search (03brr)"),
            b!(),
            t!("b = block number (1 digit)"),
            t!("rr = tape address (01–07, or store location)"),
            t!("Machine scans tape forward for block marker b"),
            t!("If EOF reached: wraps from start of tape"),
            t!("Block 0 is a convention for 'header' / init"),
            b!(),
            h!("Startup / Reset"),
            b!(),
            t!("On reset: search tape 1 for block 1 (03101)"),
            t!("Then position IP there (02101)"),
            t!("This mirrors hard-wired startup sequence"),
            b!(),
            h!("Accumulator Detail"),
            b!(),
            t!("acc: signed 16-digit register"),
            t!("mul (op 5): acc += src×dest; dest zeroed"),
            t!("div (op 6): dest = acc/src; acc = remainder"),
            t!("  — requires acc ≠ +0 before divide"),
            t!("  — dest must be 0 before divide"),
            t!("08 as src: gives low 7 of acc, scaled ×10⁸"),
            t!("08 as dst: writes low 7 of acc, scale ×10⁻⁸"),
        ],
        _ => vec![Line::from("")],
    }
}

fn render_ref_panel(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Ref;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let page = app.ref_page.min(REF_PAGE_COUNT - 1);
    let title = format!(" Reference  {}/{} ", page + 1, REF_PAGE_COUNT);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = ref_page_lines(page);
    let nav = Line::from(Span::styled(
        "  j/k ↑/↓ = prev/next page",
        Style::default().fg(Color::DarkGray),
    ));
    let mut all = lines;
    // pad so nav hint sits at bottom if room
    let inner_h = inner.height as usize;
    while all.len() + 1 < inner_h {
        all.push(Line::from(""));
    }
    all.push(nav);

    f.render_widget(Paragraph::new(Text::from(all)), inner);
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

pub fn run_tui(debugger: Debugger, startup_msgs: Vec<String>) -> Result<(), Box<dyn Error>> {
    // Disable ANSI colors inside TUI — ratatui applies its own styles
    colored::control::set_override(false);

    let mut terminal = setup_terminal()?;
    let mut app = App::new(debugger);
    for msg in startup_msgs {
        app.push_log(msg);
    }
    app.debugger.execute("set autodis false");
    app.debugger.execute("set autowhat false");

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

fn run_fzf_and_load(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    target: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    // Disable raw mode so fzf can manage the terminal itself.
    // Stay in the alternate screen — fzf --height draws over the bottom of it.
    disable_raw_mode()?;

    let result = std::process::Command::new("fzf")
        .args(["--height=40%", "--layout=reverse", "--border"])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output();

    // Restore raw mode and force a full redraw over whatever fzf left behind.
    enable_raw_mode()?;
    terminal.clear()?;

    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // fzf not installed — fall back to text prompt
            app.load_prompt = Some(target);
            app.load_prompt_buf.clear();
        }
        Err(e) => return Err(Box::new(e)),
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() { app.load_path(&path, target); }
        }
        Ok(_) => {} // user cancelled (Esc)
    }
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<(), Box<dyn Error>> {
    loop {
        // Auto-scroll tape cursors to IP if machine advanced
        sync_tape_cursors(app);

        terminal.draw(|f| render(f, app))?;

        // fzf selection (must happen outside draw, with terminal access)
        if let Some(target) = app.fzf_request.take() {
            if let Err(e) = run_fzf_and_load(terminal, app, target) {
                app.push_log(format!("fzf error: {}", e));
            }
            continue;
        }

        if app.running && !app.debugger.machine.halted {
            for _ in 0..STEPS_PER_TICK {
                if event::poll(Duration::ZERO)? { break; }
                app.step_once();
                if !app.running || app.debugger.machine.halted { break; }
            }
            if event::poll(Duration::ZERO)?
                && app.handle_event(event::read()?) {
                    break;
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
        if tape_idx < 7
            && let Some(tape) = app.debugger.machine.tapes[tape_idx].as_ref() {
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
