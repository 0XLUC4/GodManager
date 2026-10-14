//! One window only: a second start (a magnet link clicked in the browser, a .torrent opened in
//! Explorer) hands its arguments to the running program and quits.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const PORT: u16 = 47_913;
const HELLO: &str = "GODMANAGER1";
/// Argument of the program started by an update: it waits for the old one to quit.
pub const AFTER_UPDATE: &str = "--after-update";

/// Arguments received from later starts. An empty list asks the window to come forward.
#[derive(Clone, Default)]
pub struct Inbox {
    messages: Arc<Mutex<Vec<Vec<String>>>>,
    ctx: Arc<OnceLock<egui::Context>>,
}

impl Inbox {
    /// Lets the listener wake the window when something arrives.
    pub fn attach(&self, ctx: &egui::Context) {
        let _ = self.ctx.set(ctx.clone());
    }

    pub fn take(&self) -> Vec<Vec<String>> {
        self.messages
            .lock()
            .map(|mut messages| std::mem::take(&mut *messages))
            .unwrap_or_default()
    }

    fn push(&self, args: Vec<String>) {
        if let Ok(mut messages) = self.messages.lock() {
            messages.push(args);
        }
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }
}

pub enum Start {
    /// This is the first window; later starts land in the inbox.
    First(Inbox),
    /// Another window took the arguments.
    Forwarded,
}

pub fn claim(args: &[String]) -> Start {
    let inbox = Inbox::default();
    let mut bound = TcpListener::bind((Ipv4Addr::LOCALHOST, PORT));
    if args.iter().any(|arg| arg == AFTER_UPDATE) {
        for _ in 0..50 {
            if bound.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
            bound = TcpListener::bind((Ipv4Addr::LOCALHOST, PORT));
        }
        if bound.is_err() {
            return Start::First(inbox);
        }
    }
    match bound {
        Ok(listener) => {
            let receiver = inbox.clone();
            let _ = std::thread::Builder::new()
                .name("instance".into())
                .spawn(move || listen(&listener, &receiver));
            Start::First(inbox)
        }
        Err(_) if forward(args).is_ok() => Start::Forwarded,
        // The port is taken by something else: run on our own.
        Err(_) => Start::First(inbox),
    }
}

fn listen(listener: &TcpListener, inbox: &Inbox) {
    for stream in listener.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut lines = BufReader::new(stream).lines().map_while(Result::ok);
        if lines.next().as_deref() != Some(HELLO) {
            continue;
        }
        inbox.push(lines.filter(|line| !line.is_empty()).collect());
    }
}

fn forward(args: &[String]) -> std::io::Result<()> {
    let mut stream =
        TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, PORT).into(), Duration::from_secs(2))?;
    let mut message = format!("{HELLO}\n");
    for arg in args {
        message.push_str(&arg.replace(['\r', '\n'], ""));
        message.push('\n');
    }
    stream.write_all(message.as_bytes())
}
