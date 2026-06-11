use crate::types::OrbitEvent;
use crate::types::RunPhase;
use std::io::{IsTerminal, Write};
use std::time::Instant;

pub const RST: &str = "\x1b[0m";
pub const BLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const RED: &str = "\x1b[31m";
pub const GRN: &str = "\x1b[32m";
pub const YLW: &str = "\x1b[33m";
pub const CYN: &str = "\x1b[36m";
pub const BWHT: &str = "\x1b[1;37m";
const SPINNER: &[char] = &['🌑', '🌒', '🌓', '🌔', '🌕', '🌖', '🌗', '🌘'];
const GOLD: [u8; 10] = [172, 178, 184, 214, 220, 221, 222, 228, 221, 214];
const BUF_MAX: usize = 5;

fn tty() -> bool {
    std::io::stdout().is_terminal()
}

pub fn c(s: &str, ansi: &str) -> String {
    if tty() {
        format!("{}{}{}", ansi, s, RST)
    } else {
        s.to_string()
    }
}

fn gold(idx: usize) -> u8 {
    GOLD[idx % GOLD.len()]
}

pub fn print_banner(version: &str) {
    if tty() {
        let _ = write!(std::io::stdout(), "\x1b[2J\x1b[H");
        let _ = std::io::stdout().flush();
    }
    let art = vec![
        " ██████╗ ██████╗ ██████╗ ██╗████████╗",
        "██╔═══██╗██╔══██╗██╔══██╗██║╚══██╔══╝",
        "██║   ██║██████╔╝██████╔╝██║   ██║   ",
        "██║   ██║██╔══██╗██╔══██╗██║   ██║   ",
        "╚██████╔╝██║  ██║██████╔╝██║   ██║   ",
        " ╚═════╝ ╚═╝  ╚═╝╚═════╝ ╚═╝   ╚═╝   ",
    ];
    let max_w = art[0].chars().count();
    let ver_pad = " ".repeat(max_w.saturating_sub(version.len()) / 2);
    for line in &art {
        let _ = writeln!(std::io::stdout(), "  {}", c(line, BWHT));
    }
    let _ = writeln!(std::io::stdout(), "  {}{}", ver_pad, c(version, DIM));
}

pub struct Renderer {
    verbose: bool,
    buffer: Vec<String>,
    spin_idx: usize,
    stream_msg: Option<&'static str>,
    stream_start: Option<Instant>,
    work_start: Option<Instant>,
    last_buf_count: usize,
    thinking_text: String,
    think_pos: usize,
}

fn phase_sep(label: &str) -> String {
    let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
    format!("{} {} {}", c("─", DIM), c(&format!("{}  {}", label, ts), DIM), c("─", DIM))
}

impl Renderer {
    pub fn new(verbose: bool) -> Self {
        Self {
            verbose,
            buffer: Vec::with_capacity(BUF_MAX),
            spin_idx: 0,
            stream_msg: None,
            stream_start: None,
            work_start: None,
            last_buf_count: 0,
            thinking_text: String::new(),
            think_pos: 0,
        }
    }

    pub fn tick(&mut self) {
        if self.verbose || !tty() {
            return;
        }
        self.spin_idx += 1;
        let spin = SPINNER[self.spin_idx % SPINNER.len()].to_string();

        if !self.thinking_text.is_empty() && self.stream_start.is_none() {
            self.think_pos = self.think_pos.saturating_add(2);
        }

        let active = self.build_active(&spin);
        let total = self.buffer.len() + active.is_some() as usize;

        for _ in 0..self.last_buf_count {
            let _ = write!(std::io::stdout(), "\x1b[A\x1b[K");
        }

        for line in &self.buffer {
            let _ = writeln!(std::io::stdout(), "{}", line);
        }
        if let Some(a) = &active {
            let _ = writeln!(std::io::stdout(), "{}", a);
        }

        self.last_buf_count = total;
        let _ = std::io::stdout().flush();
    }

    fn marquee(&self) -> String {
        let text_len = self.thinking_text.chars().count();
        if text_len == 0 {
            return String::new();
        }
        const W: usize = 70;
        let pos = self.think_pos.min(text_len + W);
        let end = pos.min(text_len);
        let start = if end < W { 0 } else { end - W };
        let window: String = self.thinking_text.chars().skip(start).take(W).collect();
        let wlen = window.chars().count();
        if wlen == 0 {
            return String::new();
        }
        window.chars().enumerate().map(|(i, ch)| {
            let level = 255u8 - ((i as f64 / (wlen as f64 - 1.0).max(1.0)) * 18.0) as u8;
            format!("\x1b[38;5;{}m{}\x1b[0m", level.max(237).min(255), ch)
        }).collect()
    }

    fn build_active(&mut self, spin: &str) -> Option<String> {
        let start = self.stream_start.or(self.work_start)?;
        let msg = self.stream_msg.unwrap_or("working");
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let dur = if ms >= 1000.0 {
            format!("{:.1}s", ms / 1000.0)
        } else {
            format!("{:.0}ms", ms)
        };
        let c1 = gold(self.spin_idx);
        let c2 = gold(self.spin_idx.wrapping_add(3));
        let arrow = format!("\x1b[38;5;{}m▸\x1b[0m", c1);
        let orb = format!("\x1b[38;5;{}m{}\x1b[0m", c2, spin);
        let base = format!("  {} {} {}", arrow, orb, c(&format!("{} ({})", msg, dur), DIM));
        if !self.thinking_text.is_empty() && self.stream_start.is_none() {
            Some(format!("{} {}", base, self.marquee()))
        } else {
            Some(base)
        }
    }

    pub fn handle(&mut self, event: OrbitEvent) {
        if self.verbose {
            self.handle_verbose(event);
            return;
        }

        match &event {
            OrbitEvent::AgentChunk(msg) if msg.starts_with("[thought]") => {
                let body = msg.trim_start_matches("[thought] ").trim().to_string();
                if !body.is_empty() {
                    self.thinking_text.push_str(&body);
                    self.thinking_text.push(' ');
                }
                return;
            }
            OrbitEvent::ToolCall { name, params, .. } => {
                let line = match params {
                    Some(p) => {
                        let sep = if p.starts_with('(') { "" } else { "  " };
                        let dim = if tty() { format!("{}{}{}", DIM, p, RST) } else { p.clone() };
                        format!("  {} {}{}{}", c("│", DIM), c(name, CYN), sep, dim)
                    }
                    None => format!("  {} {}", c("│", DIM), c(name, CYN)),
                };
                self.push(line);
                return;
            }
            OrbitEvent::TaskMessage { task_id, message } => {
                let summary = message.lines().next().unwrap_or(message).trim();
                let truncated = if summary.len() > 70 {
                    format!("{}...", &summary[..67])
                } else {
                    summary.to_string()
                };
                self.push(format!("  {} {}  {}", c("●", CYN), c(&task_id, BLD), c(&truncated, DIM)));
                return;
            }
            OrbitEvent::AgentChunk(_) => {
                if self.stream_start.is_none() {
                    self.flush_buffer();
                    self.stream_start = Some(Instant::now());
                    if self.stream_msg.is_none() {
                        self.stream_msg = Some("processing");
                    }
                }
                return;
            }
            OrbitEvent::PhaseChanged(phase) => {
                self.flush_buffer();
                self.work_start = Some(Instant::now());
                self.thinking_text.clear();
                self.think_pos = 0;
                self.stream_msg = Some(match phase {
                    RunPhase::Prompting => "generating prompt",
                    RunPhase::Coding => "implementing",
                    RunPhase::Evaluating => "evaluating",
                    RunPhase::Done => "done",
                    RunPhase::GitPlanning => "planning commit",
                    RunPhase::GitReviewing => "reviewing plan",
                    RunPhase::GitCommitting => "committing",
                });
                let label = match phase {
                    RunPhase::Prompting => "PROMPTING",
                    RunPhase::Coding => "CODING",
                    RunPhase::Evaluating => "EVALUATING",
                    RunPhase::Done => "DONE",
                    RunPhase::GitPlanning => "GIT PLAN",
                    RunPhase::GitReviewing => "GIT REVIEW",
                    RunPhase::GitCommitting => "GIT COMMIT",
                };
                let _ = writeln!(std::io::stdout(), "{}", phase_sep(label));
            }
            OrbitEvent::RunStarted { spec_path, target } => {
                let _ = writeln!(std::io::stdout(), "  {} {}  {}", c("●", DIM), c("spec:", DIM), c(&spec_path, DIM));
                let _ = writeln!(std::io::stdout(), "  {} {}  {}", c("●", DIM), c("target:", DIM), c(&target, DIM));
            }
            _ => {
                self.flush_buffer();
                self.stream_msg = None;
                self.stream_start = None;
                self.work_start = None;
                self.thinking_text.clear();
                self.render_static(event);
            }
        }
    }

    fn handle_verbose(&mut self, event: OrbitEvent) {
        self.flush_buffer();
        self.stream_msg = None;
        self.stream_start = None;
        self.work_start = None;
        self.thinking_text.clear();

        match &event {
            OrbitEvent::AgentChunk(msg) if msg.starts_with("[thought]") => {
                let body = msg.trim_start_matches("[thought] ").trim();
                if !body.is_empty() {
                    let _ = writeln!(std::io::stdout(), "  {} {}", c("▸", YLW), c(body, YLW));
                }
                return;
            }
            OrbitEvent::AgentChunk(msg) => {
                for line in msg.lines() {
                    let t = line.trim();
                    if !t.is_empty() {
                        let _ = writeln!(std::io::stdout(), "  {} {}", c("▸", BWHT), c(t, BWHT));
                    }
                }
                return;
            }
            OrbitEvent::ToolCall { name, params, .. } => {
                match params {
                    Some(p) => {
                        let sep = if p.starts_with('(') { "" } else { "  " };
                        let dim = if tty() { format!("{}{}{}", DIM, p, RST) } else { p.clone() };
                        let _ = writeln!(std::io::stdout(), "  {} {}{}{}", c("│", DIM), c(name, CYN), sep, dim);
                    }
                    None => {
                        let _ = writeln!(std::io::stdout(), "  {} {}", c("│", DIM), c(name, CYN));
                    }
                }
                return;
            }
            _ => {}
        }

        if let OrbitEvent::PhaseChanged(phase) = &event {
            let label = match phase {
                RunPhase::Prompting => "PROMPTING",
                RunPhase::Coding => "CODING",
                RunPhase::Evaluating => "EVALUATING",
                RunPhase::Done => "DONE",
                RunPhase::GitPlanning => "GIT PLAN",
                RunPhase::GitReviewing => "GIT REVIEW",
                RunPhase::GitCommitting => "GIT COMMIT",
            };
            let _ = writeln!(std::io::stdout(), "{}", phase_sep(label));
        } else {
            self.render_static(event);
        }
    }

    fn push(&mut self, mut line: String) {
        if self.buffer.len() >= BUF_MAX {
            self.buffer.remove(0);
        }
        let visible = line.chars().filter(|&c| c != '\x1b' && c != '[').count();
        if visible > 90 {
            let mut count = 0;
            let mut truncated = String::with_capacity(line.len());
            for ch in line.chars() {
                if ch == '\x1b' || ch == '[' {
                    truncated.push(ch);
                } else if count < 87 {
                    truncated.push(ch);
                    count += 1;
                } else {
                    truncated.push('…');
                    break;
                }
            }
            line = truncated;
        }
        self.buffer.push(line);
    }

    fn flush_buffer(&mut self) {
        if !tty() || self.verbose {
            self.buffer.clear();
            self.last_buf_count = 0;
            return;
        }
        let count = self.last_buf_count.max(self.buffer.len() + self.stream_start.map_or(0, |_| 1));
        for _ in 0..count {
            let _ = write!(std::io::stdout(), "\x1b[A\x1b[K");
        }
        let _ = std::io::stdout().flush();
        self.buffer.clear();
        self.last_buf_count = 0;
    }

    fn section(&self, title: &str) {
        let _ = writeln!(std::io::stdout(), "  {} {}", c("───", DIM), c(title, BLD));
    }

    fn render_static(&mut self, event: OrbitEvent) {
        match event {
            OrbitEvent::AgentChunk(_) => {}
            OrbitEvent::ToolCall { .. } => {}
            OrbitEvent::PromptCreated { prompt_summary, rubric } => {
                self.section("GOAL");
                let _ = writeln!(std::io::stdout(), "  {} {}", c("▸", GRN), c(&prompt_summary, BWHT));
                self.section("RUBRIC");
                for r in &rubric {
                    let mark = match r.weight {
                        3 => "!!!",
                        2 => "!! ",
                        _ => "!  ",
                    };
                    let _ = writeln!(std::io::stdout(), "  {} {} {}", c(mark, YLW), c(&r.criterion, BLD), c(&r.description, DIM));
                }
            }
            OrbitEvent::CoderOutput { summary } => {
                self.section("RESULT");
                let _ = writeln!(std::io::stdout(), "  {} {}", c("▸", CYN), c(&summary, BWHT));
            }
            OrbitEvent::EvalVerdict { approved, feedback, diagnosis, results } => {
                if !results.is_empty() {
                    self.section("CRITERIA");
                    let pad = results.iter().map(|r| r.criterion.len()).max().unwrap_or(0);
                    for r in &results {
                        let sym = if r.pass { c("✓", GRN) } else { c("✗", RED) };
                        let p = format!("{:width$}", r.criterion, width = pad);
                        let _ = writeln!(std::io::stdout(), "  {} {}  {}", sym, c(&p, BLD), c(&r.evidence, DIM));
                    }
                }
                self.section("VERDICT");
                if approved {
                    let _ = writeln!(std::io::stdout(), "  {} {}", c("●", GRN), c("APPROVED", BLD));
                    let _ = writeln!(std::io::stdout(), "  {} {}", c("  ", DIM), c(&diagnosis, DIM));
                } else {
                    let _ = writeln!(std::io::stdout(), "  {} {}", c("●", RED), c("REJECTED", BLD));
                    let _ = writeln!(std::io::stdout(), "  {} {}", c("  ", DIM), c(&feedback, YLW));
                    let _ = writeln!(std::io::stdout(), "  {} {}", c("  ", DIM), c(&diagnosis, DIM));
                }
            }
            OrbitEvent::RunFinished { exit_code } => {
                let _ = writeln!(std::io::stdout(), "  {} {} {}", c("●", GRN), c("FINISHED", BLD), c(&format!("exit:{}", exit_code), DIM));
            }
            OrbitEvent::RunFailed { reason } => {
                let _ = writeln!(std::io::stdout(), "  {} {}", c("✗", RED), c(&reason, RED));
            }
            OrbitEvent::TaskMessage { task_id, message } => {
                for line in message.lines() {
                    let t = line.trim();
                    if !t.is_empty() {
                        let _ = writeln!(std::io::stdout(), "  {} {}: {}", c("●", CYN), c(&task_id, BLD), c(t, CYN));
                    }
                }
            }
            _ => {}
        }
    }
}
