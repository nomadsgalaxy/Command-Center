//! The terminal screens:
//!   Welcome   what's here, and the choice of Install, Update or Remove
//!   Monitors  on a host, which monitors to share and whether to announce
//!   Progress  the steps, each ticked off as it finishes, with the log a key away
//!   Done      what to do next
//!   Failed    what broke and what to do about it
//! App holds the state and takes the keys and the worker's events. draw() only paints it.
use crate::plan::{self, Action, Facts, Kind, Step};
use crate::{Ev, Log};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Screen {
    Welcome,
    Monitors,
    Progress,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum St {
    Waiting,
    Running,
    Done,
    Skipped(String),
    Failed,
}

/// What a key asks the main loop to do.
#[derive(Debug, PartialEq)]
pub enum Out {
    Start,
    Quit,
    Reboot,
    Pair,
}

pub struct App {
    pub f: Facts,
    pub screen: Screen,
    pub sel: usize,
    pub actions: Vec<Action>,
    pub action: Action,
    pub pick: Vec<bool>,
    pub announce: bool,
    pub steps: Vec<Step>,
    pub state: Vec<St>,
    pub log: Arc<Log>,
    pub show_log: bool,
    pub restart: bool,
    pub restart_said: bool,
    pub quit_armed: bool,
    pub note: String,
    pub tick: usize,
    pub work: PathBuf,
}

impl App {
    pub fn new(f: Facts, log: Arc<Log>, work: PathBuf) -> App {
        let actions = plan::actions(&f);
        let pick = plan::ticked(&f);
        let announce = f.announcing || f.installed.is_none();
        App { action: actions[0], actions, pick, announce, f, screen: Screen::Welcome, sel: 0, steps: vec![], state: vec![], log, show_log: false, restart: false, restart_said: false, quit_armed: false, note: String::new(), tick: 0, work }
    }

    pub fn share(&self) -> Vec<usize> {
        self.pick.iter().enumerate().filter(|(_, t)| **t).map(|(i, _)| i).collect()
    }

    fn start(&mut self) -> Option<Out> {
        self.steps = plan::plan(&self.f, self.action, &self.share(), self.announce, &self.work);
        self.state = vec![St::Waiting; self.steps.len()];
        self.screen = Screen::Progress;
        Some(Out::Start)
    }

    pub fn key(&mut self, k: KeyCode) -> Option<Out> {
        self.note.clear();
        if k == KeyCode::Char('l') && matches!(self.screen, Screen::Progress | Screen::Done | Screen::Failed) {
            self.show_log = !self.show_log;
            return None;
        }
        if matches!(k, KeyCode::Char('q') | KeyCode::Esc) {
            if self.screen == Screen::Progress && !self.quit_armed {
                self.quit_armed = true;
                self.note = "Still working. Press q again to stop it here; running the installer again picks up where it stopped.".into();
                return None;
            }
            return Some(Out::Quit);
        }
        let up = |s: &mut usize, n: usize| *s = (*s + n - 1) % n;
        let down = |s: &mut usize, n: usize| *s = (*s + 1) % n;
        match self.screen {
            Screen::Welcome if self.f.missing.is_empty() => match k {
                KeyCode::Up => up(&mut self.sel, self.actions.len()),
                KeyCode::Down => down(&mut self.sel, self.actions.len()),
                KeyCode::Enter => {
                    self.action = self.actions[self.sel];
                    if self.f.kind == Kind::Host && self.action != Action::Remove {
                        self.screen = Screen::Monitors;
                        self.sel = 0;
                    } else {
                        return self.start();
                    }
                }
                _ => {}
            },
            Screen::Monitors => {
                let n = self.pick.len() + 2; // the monitors, plus announcing and Continue
                match k {
                    KeyCode::Up => up(&mut self.sel, n),
                    KeyCode::Down => down(&mut self.sel, n),
                    KeyCode::Enter | KeyCode::Char(' ') if self.sel < self.pick.len() => self.pick[self.sel] = !self.pick[self.sel],
                    KeyCode::Enter | KeyCode::Char(' ') if self.sel == self.pick.len() => self.announce = !self.announce,
                    KeyCode::Enter if self.share().is_empty() => self.note = "Tick at least one monitor to share.".into(),
                    KeyCode::Enter => return self.start(),
                    _ => {}
                }
            }
            Screen::Done if self.restart && !self.restart_said => match k {
                KeyCode::Char('y') => return Some(Out::Reboot),
                KeyCode::Char('n') => self.restart_said = true,
                _ => {}
            },
            Screen::Done if self.f.kind == Kind::Host && self.action != Action::Remove && k == KeyCode::Char('p') => return Some(Out::Pair),
            _ => {}
        }
        None
    }

    pub fn event(&mut self, e: Ev) {
        match e {
            Ev::Started(i) => self.state[i] = St::Running,
            Ev::Finished(i, Ok(())) => self.state[i] = St::Done,
            Ev::Finished(i, Err(_)) => {
                self.state[i] = St::Failed;
                self.screen = Screen::Failed;
            }
            Ev::Skipped(i, why) => self.state[i] = St::Skipped(why),
            Ev::Restart(r) => self.restart = r,
            Ev::AllDone => {
                if self.screen != Screen::Failed {
                    self.screen = Screen::Done;
                }
            }
        }
    }
}

const ACCENT: Color = Color::Cyan;

fn dim(s: impl Into<String>) -> Line<'static> {
    Line::styled(s.into(), Style::new().fg(Color::DarkGray))
}

fn bold(s: impl Into<String>) -> Line<'static> {
    Line::styled(s.into(), Style::new().add_modifier(Modifier::BOLD))
}

fn choice(on: bool, text: String) -> Line<'static> {
    if on {
        Line::from(vec![Span::styled("> ", Style::new().fg(ACCENT)), Span::styled(text, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))])
    } else {
        Line::from(format!("  {text}"))
    }
}

pub fn draw(fr: &mut Frame, app: &App) {
    let block = Block::new().borders(Borders::ALL).border_style(Style::new().fg(ACCENT)).title(" Command Center installer ");
    let inner = block.inner(fr.area());
    fr.render_widget(block, fr.area());
    let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);
    let (lines, keys) = if app.show_log { log_lines(app, body.height as usize) } else { screen(app) };
    fr.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    let mut f = vec![];
    if !app.note.is_empty() {
        f.push(Line::styled(app.note.clone(), Style::new().fg(Color::Yellow)));
    }
    f.push(dim(keys));
    fr.render_widget(Paragraph::new(f), foot);
}

fn log_lines(app: &App, h: usize) -> (Vec<Line<'static>>, String) {
    let all = app.log.lines();
    let mut v = vec![bold("Log"), dim(format!("Also saved in {}", app.log.path.display()))];
    v.extend(all[all.len().saturating_sub(h.saturating_sub(3))..].iter().map(|l| Line::from(l.clone())));
    (v, "l: back   q: quit".into())
}

fn screen(app: &App) -> (Vec<Line<'static>>, String) {
    let f = &app.f;
    let mut v: Vec<Line<'static>> = vec![];
    match app.screen {
        Screen::Welcome => {
            v.push(bold(match f.kind {
                Kind::Frame => "This is a Steam Frame.".to_owned(),
                Kind::Host => format!("This is {}, a computer the Frame can show ({}).", f.host, f.arch),
            }));
            v.push(Line::from(match &f.installed {
                Some(ver) => format!("Command Center is installed here: {ver}."),
                None => "Command Center isn't installed here yet.".into(),
            }));
            if f.kind == Kind::Frame && f.desktop_open {
                v.push(Line::from("Your Desktop is open. It keeps running while this works, and an update loads the next time you open it."));
            }
            v.push(Line::default());
            if !f.missing.is_empty() {
                v.push(bold("Before I can install, these need fixing:"));
                for (what, fix) in &f.missing {
                    v.push(Line::styled(format!("  ✗ {what}"), Style::new().fg(Color::Red)));
                    v.push(Line::from(format!("    {fix}")));
                }
                v.push(Line::default());
                v.push(Line::from("Then run the installer again."));
                return (v, "q: quit".into());
            }
            v.push(Line::from("What would you like to do?"));
            for (i, a) in app.actions.iter().enumerate() {
                let what = match (a, f.kind) {
                    (Action::Install, Kind::Frame) => "Install: get the code, build it in a container, and make Desktop open Command Center",
                    (Action::Update, Kind::Frame) => "Update: get the latest code and build it again",
                    (Action::Remove, Kind::Frame) => "Remove: give Desktop back to SteamOS and unregister the pointer",
                    (Action::Install, Kind::Host) => "Install: share this computer's monitors with the Frame",
                    (Action::Update, Kind::Host) => "Update: get the latest cc-host, and change which monitors are shared",
                    (Action::Remove, Kind::Host) => "Remove: stop sharing and remove cc-host",
                };
                v.push(choice(i == app.sel, what.to_owned()));
            }
            (v, "↑↓: choose   Enter: go   q: quit".into())
        }
        Screen::Monitors => {
            v.push(bold("Which monitors should the Frame see?"));
            v.push(dim("Each one shows up on the Frame as its own screen."));
            v.push(Line::default());
            for (i, m) in f.monitors.iter().enumerate() {
                v.push(choice(app.sel == i, format!("[{}] {i}  {}  {}x{}", if app.pick[i] { "x" } else { " " }, m.name, m.w, m.h)));
            }
            v.push(Line::default());
            let n = app.pick.len();
            v.push(choice(app.sel == n, format!("[{}] Let the Frame find this computer on the network", if app.announce { "x" } else { " " })));
            v.push(dim("    It announces this computer's name and its monitors' sizes on your local network (mDNS)."));
            v.push(dim("    Without it, the Frame doesn't list it, and you pair from a terminal on the Frame instead."));
            v.push(Line::default());
            v.push(choice(app.sel == n + 1, format!("{} with {}", app.action.label(), plan::monitor_list(&app.share()))));
            (v, "↑↓: move   Enter or Space: tick   Enter on the last line: go   q: quit".into())
        }
        Screen::Progress | Screen::Failed => {
            v.push(bold(format!("{}ing Command Center{}", app.action.label().trim_end_matches('e'), if f.kind == Kind::Host { format!(" on {}", f.host) } else { String::new() })));
            v.push(Line::default());
            let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][app.tick % 10];
            for (s, st) in app.steps.iter().zip(&app.state) {
                v.push(match st {
                    St::Waiting => dim(format!("  ·  {}", s.title)),
                    St::Running => Line::from(vec![Span::styled(format!("  {spin}  "), Style::new().fg(ACCENT)), Span::raw(s.title.clone())]),
                    St::Done => Line::from(vec![Span::styled("  ✓  ", Style::new().fg(Color::Green)), Span::raw(s.title.clone())]),
                    St::Skipped(why) => Line::from(vec![Span::styled("  ✓  ", Style::new().fg(Color::Green)), Span::raw(format!("{} (skipped: {why})", s.title))]),
                    St::Failed => Line::from(vec![Span::styled("  ✗  ", Style::new().fg(Color::Red)), Span::raw(s.title.clone())]),
                });
            }
            v.push(Line::default());
            if app.screen == Screen::Failed {
                let i = app.state.iter().position(|s| *s == St::Failed).unwrap_or(0);
                v.push(Line::styled(format!("\"{}\" didn't work.", app.steps[i].title), Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)));
                v.push(Line::from(app.steps[i].fix.clone()));
                v.push(Line::default());
                v.push(dim("The last lines of the log:"));
                let all = app.log.lines();
                v.extend(all[all.len().saturating_sub(8)..].iter().map(|l| dim(format!("  {l}"))));
                return (v, format!("l: the whole log   q: quit   (the log is saved in {})", app.log.path.display()));
            }
            if let Some(last) = app.log.lines().last() {
                v.push(dim(format!("  {last}")));
            }
            (v, "l: log   q: quit".into())
        }
        Screen::Done => {
            let lines = plan::done(f, app.action, &app.share(), app.announce, &app.log.lines(), app.restart);
            v.push(Line::styled(lines[0].clone(), Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)));
            v.extend(lines[1..].iter().map(|l| Line::from(l.clone())));
            if app.restart && !app.restart_said {
                v.push(Line::default());
                v.push(bold("Restart the Frame now? Everything open closes, including games and the Desktop."));
                return (v, "y: restart now   n: later   l: log   q: quit".into());
            }
            if app.restart {
                v.push(Line::from("Okay, restart it whenever it suits you."));
            }
            if f.kind == Kind::Host && app.action != Action::Remove {
                return (v, "p: show the pairing key now (cc-share pair)   l: log   q: quit".into());
            }
            (v, "l: log   q: quit".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::tests::{frame, host};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app(f: Facts) -> App {
        App::new(f, Arc::new(Log::memory()), "/tmp/w".into())
    }

    /// The screen as text, the way the terminal shows it.
    pub fn shot(app: &App) -> String {
        let mut t = Terminal::new(TestBackend::new(100, 24)).unwrap();
        t.draw(|fr| draw(fr, app)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>().trim_end().to_owned()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn host_install_walks_through_the_monitors() {
        let mut a = app(host());
        assert!(shot(&a).contains("This is desk, a computer the Frame can show (x86_64)."));
        assert_eq!(a.key(KeyCode::Enter), None);
        assert_eq!(a.screen, Screen::Monitors);
        assert_eq!(a.share(), vec![0, 1], "every monitor ticked at first");
        a.key(KeyCode::Enter); // untick 0
        a.key(KeyCode::Down);
        a.key(KeyCode::Char(' ')); // untick 1
        a.key(KeyCode::Down);
        a.key(KeyCode::Down);
        assert_eq!(a.key(KeyCode::Enter), None, "nothing ticked");
        assert!(a.note.contains("at least one"));
        a.key(KeyCode::Up);
        a.key(KeyCode::Enter); // announcing off
        a.key(KeyCode::Up);
        a.key(KeyCode::Enter); // 1 back on
        a.key(KeyCode::Down);
        a.key(KeyCode::Down);
        assert_eq!(a.key(KeyCode::Enter), Some(Out::Start));
        assert_eq!(a.share(), vec![1]);
        assert!(!a.announce);
        assert_eq!(a.steps.len(), 2);
    }

    #[test]
    fn missing_prerequisites_block_the_install() {
        let mut f = host();
        f.missing.push(("krdpserver".into(), "Install it with: sudo pacman -S --needed krdp".into()));
        let mut a = app(f);
        assert_eq!(a.key(KeyCode::Enter), None);
        assert_eq!(a.screen, Screen::Welcome);
        assert!(shot(&a).contains("sudo pacman -S --needed krdp"));
        assert_eq!(a.key(KeyCode::Char('q')), Some(Out::Quit));
    }

    #[test]
    fn quitting_while_working_asks_twice() {
        let mut a = app(frame());
        assert_eq!(a.key(KeyCode::Enter), Some(Out::Start));
        assert_eq!(a.key(KeyCode::Char('q')), None);
        assert_eq!(a.key(KeyCode::Char('q')), Some(Out::Quit));
    }

    #[test]
    fn the_restart_is_offered_never_assumed() {
        let mut a = app(frame());
        a.key(KeyCode::Enter);
        for i in 0..a.steps.len() {
            a.event(Ev::Finished(i, Ok(())));
        }
        a.event(Ev::Restart(true));
        a.event(Ev::AllDone);
        assert_eq!(a.screen, Screen::Done);
        assert_eq!(a.key(KeyCode::Enter), None);
        assert_eq!(a.key(KeyCode::Char('p')), None);
        assert_eq!(a.key(KeyCode::Char('n')), None);
        assert_eq!(a.key(KeyCode::Char('y')), None, "after a no, y does nothing");
        assert!(shot(&a).contains("whenever it suits you"));
    }

    #[test]
    fn a_failure_says_what_broke() {
        let mut a = app(frame());
        a.key(KeyCode::Enter);
        a.event(Ev::Finished(0, Ok(())));
        a.log.push("error: failed to download fedora-toolbox:44".into());
        a.event(Ev::Finished(1, Err("exit 1".into())));
        a.event(Ev::AllDone);
        assert_eq!(a.screen, Screen::Failed);
        let s = shot(&a);
        assert!(s.contains("\"Set up the build container (the first time takes a while)\" didn't work.") && s.contains("fedora-toolbox:44"));
    }

    /// Prints the screens as text. Run: cargo test -p cc-install screens -- --nocapture
    #[test]
    fn screens() {
        let mut shots = vec![];
        let mut f = frame();
        f.installed = Some("v0.1.0-12-g6fc60f0".into());
        f.desktop_open = true;
        f.repo_state = plan::Repo::Pull;
        let mut a = app(f);
        shots.push(("Frame: Welcome", shot(&a)));
        a.key(KeyCode::Enter);
        a.event(Ev::Finished(0, Ok(())));
        a.event(Ev::Finished(1, Ok(())));
        a.event(Ev::Started(2));
        a.log.push("   Compiling cc-home v0.1.0 (/home/user/control-center/crates/cc-home)".into());
        shots.push(("Frame: Progress", shot(&a)));
        for i in 2..5 {
            a.event(Ev::Finished(i, Ok(())));
        }
        a.event(Ev::Restart(true));
        a.event(Ev::AllDone);
        shots.push(("Frame: Done", shot(&a)));
        let mut a = app(host());
        a.key(KeyCode::Enter);
        shots.push(("Host: Monitors", shot(&a)));
        a.sel = 3;
        a.key(KeyCode::Enter);
        a.event(Ev::Finished(0, Ok(())));
        a.log.push("firewall: to add the pairing and paired-Frame ports, run (or pass --firewall):".into());
        a.log.push("  sudo firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=192.168.0.0/16 port port=3399-3449 protocol=tcp accept'".into());
        a.event(Ev::Finished(1, Ok(())));
        a.event(Ev::AllDone);
        shots.push(("Host: Done", shot(&a)));
        let mut a = app(host());
        a.key(KeyCode::Enter);
        a.sel = 3;
        a.key(KeyCode::Enter);
        a.log.push("curl: (6) Could not resolve host: github.com".into());
        a.event(Ev::Finished(0, Err("exit 6".into())));
        shots.push(("Host: Failed", shot(&a)));
        for (name, s) in &shots {
            println!("---- {name}\n{s}\n");
        }
        assert!(shots[2].1.contains("Restart the Frame now?"));
        assert!(shots[4].1.contains("Pick desk, press Pair"));
    }
}
