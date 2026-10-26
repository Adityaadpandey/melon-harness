//! Runs the full-screen front end: a thread owns the terminal (raw mode, alternate screen, mouse) and draws a `Screen`;
//! the async app talks to it through this handle. Submitted lines, interrupts and answers come back over channels.
use super::input::{Completions, Line};
use super::screen::{Action, Screen};
use super::sink;
use super::tui::Tui;
use crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Notify};

enum Msg {
    Text(String),
    Interrupted,
    Eof,
}

type Steer = Arc<Mutex<Vec<String>>>;

pub struct Fs {
    screen: Arc<Mutex<Screen>>,
    tx: mpsc::UnboundedSender<Msg>,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Msg>>,
    choice: Mutex<Option<oneshot::Sender<usize>>>,
    line: Mutex<Option<oneshot::Sender<String>>>,
    /// Set while a turn runs; Ctrl+C wakes it.
    cancel: Mutex<Option<Arc<Notify>>>,
    steer: Mutex<Option<Steer>>,
    on_cycle: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    stop: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
    tui: Arc<Tui>,
    keyboard_flags: AtomicBool,
}

fn slot() -> &'static Mutex<Option<Arc<Fs>>> {
    static S: OnceLock<Mutex<Option<Arc<Fs>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// The running full-screen front end, if there is one.
pub fn current() -> Option<Arc<Fs>> {
    slot().lock().unwrap().clone()
}

fn restore(flags: bool) {
    let mut o = std::io::stdout();
    if flags {
        let _ = crossterm::execute!(o, PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(o, DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

pub fn wanted() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal() && std::io::stdin().is_terminal() && std::env::var_os("MELON_PLAIN").is_none()
}

pub fn start(tui: Arc<Tui>, title: Arc<dyn Fn() -> String + Send + Sync>, footer: Arc<dyn Fn() -> String + Send + Sync>, comp: Arc<Completions>, history: Option<PathBuf>) -> Result<Arc<Fs>, String> {
    sink::install();
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut out = std::io::stdout();
    let flags = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false); // Shift+Enter arrives as itself
    crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste).map_err(|e| e.to_string())?;
    if flags {
        let _ = crossterm::execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    }
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        restore(flags);
        prev(i);
    }));
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::unbounded_channel();
    let fs = Arc::new(Fs {
        screen: Arc::new(Mutex::new(Screen::new(tui.clone(), title, footer, comp, history))),
        tx,
        rx: tokio::sync::Mutex::new(rx),
        choice: Mutex::new(None),
        line: Mutex::new(None),
        cancel: Mutex::new(None),
        steer: Mutex::new(None),
        on_cycle: Mutex::new(None),
        stop: AtomicBool::new(false),
        thread: Mutex::new(None),
        tui,
        keyboard_flags: AtomicBool::new(flags),
    });
    let f2 = fs.clone();
    *fs.thread.lock().unwrap() = Some(std::thread::spawn(move || f2.run(terminal)));
    *slot().lock().unwrap() = Some(fs.clone());
    Ok(fs)
}

impl Fs {
    fn run(self: Arc<Self>, mut terminal: Terminal<CrosstermBackend<std::io::Stdout>>) {
        let (mut last_draw, mut sig, mut mouse_on) = (Instant::now() - Duration::from_secs(1), None, true);
        let mut dirty = true;
        while !self.stop.load(Ordering::Relaxed) {
            if event::poll(Duration::from_millis(40)).unwrap_or(false) {
                while event::poll(Duration::ZERO).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                            let action = self.screen.lock().unwrap().key(k);
                            self.act(action);
                        }
                        Ok(Event::Mouse(m)) => self.screen.lock().unwrap().mouse(m),
                        Ok(Event::Paste(t)) => self.screen.lock().unwrap().paste(&t),
                        _ => {}
                    }
                }
                dirty = true;
            }
            let (animating, idle) = {
                let s = self.screen.lock().unwrap();
                let a = s.animating();
                (a, if a { None } else { Some(s.idle_sig()) })
            };
            let due = if animating { last_draw.elapsed() >= Duration::from_millis(100) } else { idle != sig };
            if dirty || due || sink::take_dirty() {
                let mut s = self.screen.lock().unwrap();
                if s.mouse_on != mouse_on {
                    mouse_on = s.mouse_on;
                    let mut o = std::io::stdout();
                    let _ = if mouse_on { crossterm::execute!(o, EnableMouseCapture) } else { crossterm::execute!(o, DisableMouseCapture) };
                }
                let _ = terminal.draw(|f| s.draw(f));
                last_draw = Instant::now();
                sig = idle;
                dirty = false;
            }
        }
    }

    fn act(&self, a: Action) {
        match a {
            Action::None => {}
            Action::Submit(text) => {
                let first = text.trim().lines().next().unwrap_or("").chars().take(80).collect::<String>();
                if self.tui.is_turn_active() {
                    if let Some(steer) = self.steer.lock().unwrap().clone() {
                        crate::uprintln!("  \x1b[2m↪ steering, read after the current tool calls:\x1b[0m {first}");
                        steer.lock().unwrap().push(text.trim().to_string());
                        return;
                    }
                    crate::uprintln!("  \x1b[2m⏳ queued, runs after this turn:\x1b[0m {first}");
                }
                let _ = self.tx.send(Msg::Text(text));
            }
            Action::Answer(text) => {
                if let Some(t) = self.line.lock().unwrap().take() {
                    let _ = t.send(text);
                }
            }
            Action::Pick(i) => {
                if let Some(t) = self.choice.lock().unwrap().take() {
                    let _ = t.send(i);
                }
            }
            Action::Interrupt => match self.cancel.lock().unwrap().as_ref() {
                Some(n) => n.notify_one(),
                None => {
                    let _ = self.tx.send(Msg::Interrupted);
                }
            },
            Action::Eof => {
                let _ = self.tx.send(Msg::Eof);
            }
            Action::Cycle => {
                if let Some(f) = self.on_cycle.lock().unwrap().clone() {
                    f();
                }
            }
        }
    }

    // -- what the app calls ----------------------------------------------------------------------------------------------
    /// The next submitted line.
    pub async fn read(&self) -> Line {
        match self.rx.lock().await.recv().await {
            Some(Msg::Text(t)) => Line::Text(t),
            Some(Msg::Interrupted) => Line::Interrupted,
            Some(Msg::Eof) | None => Line::Eof,
        }
    }

    /// Numbered options above the input, answered with one key. Esc / Ctrl+C pick the last.
    pub async fn choose(&self, question: String, options: Vec<String>) -> usize {
        let n = options.len();
        let (tx, rx) = oneshot::channel();
        *self.choice.lock().unwrap() = Some(tx);
        self.screen.lock().unwrap().question = Some((question, options));
        let pick = rx.await.unwrap_or(n - 1);
        self.screen.lock().unwrap().question = None;
        pick
    }

    /// One typed line, as the answer to a question. `secret` shows dots and keeps it out of history.
    pub async fn ask_line(&self, prompt: &str, secret: bool) -> String {
        crate::uprintln!("  \x1b[2m{}\x1b[0m", prompt.trim());
        let (tx, rx) = oneshot::channel();
        *self.line.lock().unwrap() = Some(tx);
        {
            let mut s = self.screen.lock().unwrap();
            s.line_mode = true;
            s.editor.secret = secret;
        }
        let text = rx.await.unwrap_or_default();
        let mut s = self.screen.lock().unwrap();
        s.line_mode = false;
        s.editor.secret = false;
        text
    }

    /// Ctrl+C during a turn wakes the returned Notify; call `end_turn` when it is over.
    pub fn begin_turn(&self) -> Arc<Notify> {
        let n = Arc::new(Notify::new());
        *self.cancel.lock().unwrap() = Some(n.clone());
        n
    }

    pub fn end_turn(&self) {
        *self.cancel.lock().unwrap() = None;
    }

    /// Lines typed mid-turn go here (the agent reads them after its current tool calls).
    pub fn set_steer(&self, s: Steer) {
        *self.steer.lock().unwrap() = Some(s);
    }

    pub fn set_cycle(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.on_cycle.lock().unwrap() = Some(f);
    }

    /// Put a line back as if typed (a steering note the turn ended before reading).
    pub fn queue(&self, text: String) {
        let _ = self.tx.send(Msg::Text(text));
    }

    pub fn clear(&self) {
        self.screen.lock().unwrap().clear();
    }

    pub fn all_text(&self) -> String {
        self.screen.lock().unwrap().all_text()
    }

    /// Leave the alternate screen with the conversation still on the real terminal.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
        restore(self.keyboard_flags.load(Ordering::Relaxed));
        let _ = std::panic::take_hook();
        *slot().lock().unwrap() = None;
        let text = sink::uninstall();
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(text.as_bytes());
        let _ = o.flush();
    }
}
