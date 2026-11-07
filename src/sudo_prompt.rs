//! What a plan that needs sudo asks when sudo wants a password: the password, or user mode
//! instead (the plan made again without sudo), or back. The TUI shows it as a dialog, the
//! command line as one prompt line; both run this state machine.

use std::io::Write;
use std::process::{Command, Stdio};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The prompt's controls, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Password,
    UserMode,
    Back,
}

impl Focus {
    fn next(self, user_mode: bool) -> Focus {
        match self {
            Focus::Password if user_mode => Focus::UserMode,
            Focus::Password | Focus::UserMode => Focus::Back,
            Focus::Back => Focus::Password,
        }
    }
    fn prev(self, user_mode: bool) -> Focus {
        match self {
            Focus::Password => Focus::Back,
            Focus::UserMode => Focus::Password,
            Focus::Back if user_mode => Focus::UserMode,
            Focus::Back => Focus::Password,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Action {
    /// Check this password with sudo.
    Submit(Secret),
    UserMode,
    Back,
}

/// A password: never printed, wiped from memory when dropped.
#[derive(PartialEq)]
pub struct Secret(String);

impl Secret {
    fn new() -> Secret {
        // Room for any sane password, so that typing never moves it (leaving a copy behind).
        Secret(String::with_capacity(256))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    #[cfg(test)]
    pub fn new_for_test(s: &str) -> Secret {
        let mut x = Secret::new();
        x.0.push_str(s);
        x
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Zeroes the whole buffer, also what `pop` left beyond the length.
fn wipe(s: &mut String) {
    // SAFETY: the bytes are written, never read, within the allocation; the string is then
    // emptied, so it holds no invalid UTF-8.
    unsafe {
        let v = s.as_mut_vec();
        let (p, cap) = (v.as_mut_ptr(), v.capacity());
        for i in 0..cap {
            std::ptr::write_volatile(p.add(i), 0);
        }
        v.set_len(0);
    }
}

pub struct Prompt {
    /// Items of the plan that need sudo.
    pub items: usize,
    password: Secret,
    pub focus: Focus,
    /// Why the last password did not do.
    pub error: Option<String>,
    /// A password is being checked: keys wait.
    pub checking: bool,
    /// The way out: "Back" to the plan in the TUI, "Cancel" on the command line.
    pub back_label: &'static str,
    /// User mode is offered (not when `--scope system` asked for sudo).
    pub user_mode: bool,
}

impl Prompt {
    pub fn new(items: usize, back_label: &'static str, user_mode: bool) -> Prompt {
        Prompt {
            items,
            password: Secret::new(),
            focus: Focus::Password,
            error: None,
            checking: false,
            back_label,
            user_mode,
        }
    }

    pub fn title(&self) -> String {
        match self.items {
            1 => "1 item in this plan needs sudo. Password:".into(),
            n => format!("{n} items in this plan need sudo. Password:"),
        }
    }

    /// What the password field shows.
    pub fn masked(&self) -> String {
        "•".repeat(self.password.len())
    }

    pub fn key(&mut self, k: KeyEvent) -> Option<Action> {
        if self.checking {
            return None;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => return Some(Action::Back),
            KeyCode::Char('u') if ctrl => wipe(&mut self.password.0),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.password.0.push(c);
                self.focus = Focus::Password;
                self.error = None;
            }
            KeyCode::Backspace => {
                self.password.0.pop();
                self.focus = Focus::Password;
            }
            KeyCode::Right | KeyCode::Tab => self.focus = self.focus.next(self.user_mode),
            KeyCode::Left | KeyCode::BackTab => self.focus = self.focus.prev(self.user_mode),
            KeyCode::Esc => return Some(Action::Back),
            KeyCode::Enter => match self.focus {
                Focus::Password if self.password.is_empty() => {
                    self.error = Some("Type the password, or choose user mode".into());
                }
                Focus::Password => {
                    self.checking = true;
                    self.error = None;
                    return Some(Action::Submit(std::mem::replace(
                        &mut self.password,
                        Secret::new(),
                    )));
                }
                Focus::UserMode => return Some(Action::UserMode),
                Focus::Back => return Some(Action::Back),
            },
            _ => {}
        }
        None
    }

    /// The password checked did not do: say why and take another.
    pub fn failed(&mut self, check: Check) {
        self.checking = false;
        self.focus = Focus::Password;
        self.error = Some(
            match check {
                Check::Ok => return,
                Check::Wrong => "Wrong password, try again",
                Check::Denied if self.user_mode => {
                    "sudo does not allow this user: choose user mode"
                }
                Check::Denied => "sudo does not allow this user",
            }
            .into(),
        );
    }
}

/// What sudo said to a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Accepted: sudo works without asking for a while (`sudo -n`).
    Ok,
    Wrong,
    /// Right or wrong, this user may not use sudo here.
    Denied,
}

/// Hands the password to `sudo -v` on its stdin (never on a command line or in a file).
pub fn check(password: &Secret) -> Check {
    let child = Command::new("sudo")
        .args(["-S", "-p", "", "-v"])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return Check::Denied;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let mut line = Secret::new();
        line.0.push_str(password.as_str());
        line.0.push('\n');
        let _ = stdin.write_all(line.0.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return Check::Wrong;
    };
    if out.status.success() {
        return Check::Ok;
    }
    verdict(&String::from_utf8_lossy(&out.stderr))
}

/// Tells "may not use sudo" from a wrong password in sudo's (C locale) complaint.
fn verdict(stderr: &str) -> Check {
    let denied = ["not in the sudoers", "not allowed to", "may not run sudo"];
    if denied.iter().any(|d| stderr.contains(d)) {
        Check::Denied
    } else {
        Check::Wrong
    }
}

/// What the command-line prompt ended with.
pub enum Answer {
    /// sudo took this password.
    Sudo(Secret),
    UserMode,
}

/// The controlling terminal, where someone can type the password even when stdin or stdout
/// is a pipe (`curl … | bash`, `| tee log`).
fn tty() -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
}

/// Whether someone can be asked for the password here.
pub fn can_ask() -> bool {
    tty().is_ok()
}

/// The prompt on the terminal, after the plan was printed: `Ok(None)` cancels.
pub fn ask_on_terminal(items: usize, user_mode: bool) -> std::io::Result<Option<Answer>> {
    use crossterm::{cursor, event, execute, style, terminal};
    struct Raw;
    impl Drop for Raw {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
            if let Ok(mut t) = tty() {
                let _ = execute!(t, cursor::Show);
                let _ = writeln!(t);
            }
        }
    }
    // On the terminal itself: a piped stdout (a log) never gets the prompt.
    let mut out = tty()?;
    let mut p = Prompt::new(items, "Cancel", user_mode);
    writeln!(out, "{}", p.title())?;
    terminal::enable_raw_mode()?;
    let _raw = Raw;
    loop {
        let button = |f: Focus, label: &str| {
            if p.focus == f {
                format!("\x1b[7m {label} \x1b[0m")
            } else {
                format!(" {label} ")
            }
        };
        let status = match (&p.error, p.checking) {
            (_, true) => "  checking…".to_string(),
            (Some(e), _) => format!("  \x1b[31m{e}\x1b[0m"),
            _ => String::new(),
        };
        let field = format!("  {}", p.masked());
        let user = if p.user_mode {
            format!("{}  ", button(Focus::UserMode, "Use user mode"))
        } else {
            String::new()
        };
        execute!(
            out,
            cursor::MoveToColumn(0),
            terminal::Clear(terminal::ClearType::CurrentLine),
            style::Print(format!(
                "{field}   {user}{}{status}",
                button(Focus::Back, p.back_label)
            )),
        )?;
        if p.focus == Focus::Password && !p.checking {
            let col = u16::try_from(2 + p.masked().chars().count()).unwrap_or(u16::MAX);
            execute!(out, cursor::MoveToColumn(col), cursor::Show)?;
        } else {
            execute!(out, cursor::Hide)?;
        }
        out.flush()?;
        let event::Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind != event::KeyEventKind::Press {
            continue;
        }
        match p.key(k) {
            Some(Action::Submit(pw)) => {
                // Show "checking…" while sudo takes its time over a wrong one.
                execute!(
                    out,
                    cursor::MoveToColumn(0),
                    terminal::Clear(terminal::ClearType::CurrentLine),
                    style::Print("  checking…")
                )?;
                match check(&pw) {
                    Check::Ok => return Ok(Some(Answer::Sudo(pw))),
                    other => p.failed(other),
                }
            }
            Some(Action::UserMode) => return Ok(Some(Answer::UserMode)),
            Some(Action::Back) => return Ok(None),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn typed(p: &mut Prompt, s: &str) {
        for c in s.chars() {
            assert_eq!(p.key(key(KeyCode::Char(c))), None);
        }
    }

    #[test]
    fn typing_fills_the_password_and_enter_submits_it() {
        let mut p = Prompt::new(3, "Back", true);
        assert_eq!(p.title(), "3 items in this plan need sudo. Password:");
        typed(&mut p, "pa55");
        p.key(key(KeyCode::Backspace));
        typed(&mut p, "5!");
        assert_eq!(p.masked(), "•••••");
        let Some(Action::Submit(pw)) = p.key(key(KeyCode::Enter)) else {
            panic!("not submitted");
        };
        assert_eq!(pw.as_str(), "pa55!");
        assert!(p.checking && p.masked().is_empty());
        // Keys wait for the check.
        assert_eq!(p.key(key(KeyCode::Esc)), None);
        p.failed(Check::Wrong);
        assert_eq!(p.error.as_deref(), Some("Wrong password, try again"));
        assert_eq!(p.key(key(KeyCode::Esc)), Some(Action::Back));
    }

    #[test]
    fn arrows_move_between_the_password_and_the_buttons() {
        let mut p = Prompt::new(1, "Back", true);
        assert_eq!(p.title(), "1 item in this plan needs sudo. Password:");
        // An empty password is not sent.
        assert_eq!(p.key(key(KeyCode::Enter)), None);
        assert!(p.error.is_some());
        p.key(key(KeyCode::Right));
        assert_eq!(p.focus, Focus::UserMode);
        assert_eq!(p.key(key(KeyCode::Enter)), Some(Action::UserMode));
        p.key(key(KeyCode::Tab));
        assert_eq!(p.focus, Focus::Back);
        assert_eq!(p.key(key(KeyCode::Enter)), Some(Action::Back));
        p.key(key(KeyCode::Right));
        assert_eq!(p.focus, Focus::Password);
        p.key(key(KeyCode::Left));
        assert_eq!(p.focus, Focus::Back);
        // Typing goes to the password wherever the focus is.
        typed(&mut p, "x");
        assert_eq!((p.focus, p.masked().as_str()), (Focus::Password, "•"));
        p.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(p.masked(), "");
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Back)
        );
    }

    #[test]
    fn without_user_mode_the_arrows_skip_its_button() {
        let mut p = Prompt::new(1, "Cancel", false);
        p.key(key(KeyCode::Right));
        assert_eq!(p.focus, Focus::Back);
        p.key(key(KeyCode::Left));
        assert_eq!(p.focus, Focus::Password);
        p.key(key(KeyCode::Left));
        assert_eq!(p.focus, Focus::Back);
        p.failed(Check::Denied);
        assert_eq!(p.error.as_deref(), Some("sudo does not allow this user"));
    }

    #[test]
    fn sudo_refusing_the_user_is_told_from_a_wrong_password() {
        assert_eq!(
            verdict("sudo: 1 incorrect password attempt\n"),
            Check::Wrong
        );
        assert_eq!(
            verdict("alice is not in the sudoers file.  This incident will be reported.\n"),
            Check::Denied
        );
        assert_eq!(
            verdict("Sorry, user alice may not run sudo on workstation.\n"),
            Check::Denied
        );
    }

    #[test]
    fn a_password_is_wiped_when_dropped() {
        let mut s = Secret::new();
        s.0.push_str("hunter2");
        s.0.pop();
        wipe(&mut s.0);
        assert!(s.is_empty());
        // SAFETY: within the capacity, all written by `wipe`.
        let left = unsafe { std::slice::from_raw_parts(s.0.as_ptr(), 7) };
        assert!(left.iter().all(|&b| b == 0));
    }
}
