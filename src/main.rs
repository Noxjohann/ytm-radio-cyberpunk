//! YTM Radio: a small window that shows YouTube Music, and turns what it plays into a radio stream for
//! Cyberpunk 2077's RadioExt (http://127.0.0.1:52848/stream.mp3).
//!
//! Based on Spot On Radio by Sz4bcsi (SZ4), see LICENSE-SpotOnRadio.txt.
//!
//! How the pieces talk:
//!  * page -> app: `inject.js` (runs inside the YouTube Music page) posts the page's audio and the song info
//!    through `window.ipc.postMessage`; `handle_ipc` feeds the audio into the stream and keeps the song.
//!  * app -> game: `state.txt` next to this exe (rewritten every 0.5 s); the CET mod `YtmRadio` reads it.
//!  * game -> app: the CET mod writes `cmd.txt` next to this exe; `spawn_command_watcher` runs the command in the page.
//!    (CET can only read and write files inside its own mod folder, which is why this exe lives there.)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod stream;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{WebContext, WebViewBuilder};

/// Must match streamInfo.streamURL in the RadioExt station's metadata.json and STREAM_URL in the CET mod.
const STREAM_ADDR: (&str, u16) = ("127.0.0.1", 52848);
const START_URL: &str = "https://music.youtube.com";
const INJECT: &str = include_str!("inject.js");
/// The only commands the game may send (checked before anything is run in the page).
const COMMANDS: [&str; 5] = ["next", "prev", "toggle", "pause", "play"];

enum UserEvent {
    Eval(String),
}

#[derive(Clone, Default)]
struct Song {
    title: String,
    artist: String,
    album: String,
    playing: bool,
    /// Increases whenever the song changes; the CET mod announces each value once.
    seq: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------- log file

struct FileLogger(Mutex<File>);

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut f = lock(&self.0);
        let _ = writeln!(f, "[{secs}] {} {}", record.level(), record.args());
        let _ = f.flush();
    }

    fn flush(&self) {}
}

fn init_logger(dir: &Path) {
    // Start a fresh log each run (an old one is kept as ytm-radio.old.log).
    let path = dir.join("ytm-radio.log");
    let _ = fs::rename(&path, dir.join("ytm-radio.old.log"));
    if let Ok(file) = File::create(&path) {
        if log::set_boxed_logger(Box::new(FileLogger(Mutex::new(file)))).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

// ---------------------------------------------------------------- page -> app

/// One message from the page (see the header of inject.js).
fn handle_ipc(body: &str, pcm: &stream::Pcm, song: &Mutex<Song>) {
    if let Some(b64) = body.strip_prefix("A:") {
        match base64::engine::general_purpose::STANDARD.decode(b64) {
            Ok(bytes) => {
                let samples: Vec<i16> = bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
                // Whole stereo frames only, so left and right never swap.
                let even = samples.len() & !1;
                pcm.push_live(&samples[..even]);
            }
            Err(e) => log::warn!("bad audio message from the page: {e}"),
        }
    } else if let Some(json) = body.strip_prefix("M:") {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return };
        let text = |key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_owned();
        let (title, artist, album) = (text("t"), text("a"), text("al"));
        let mut s = lock(song);
        if s.title != title || s.artist != artist {
            s.seq += 1;
            log::info!("song: {artist} - {title}");
        }
        s.title = title;
        s.artist = artist;
        s.album = album;
        s.playing = v.get("p").and_then(serde_json::Value::as_bool).unwrap_or(false);
    } else if let Some(text) = body.strip_prefix("L:") {
        log::info!("page: {text}");
    }
}

// ---------------------------------------------------------------- app -> game (state.txt)

fn clean(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

/// `v1 playing seq title artist album clients stream_down tick` (tab separated). `tick` grows every write, so the
/// CET mod can tell that this app is still running.
fn write_state(dir: &Path, song: &Song, clients: usize, stream_down: bool, tick: u64) {
    let line = format!(
        "v1\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        u8::from(song.playing),
        song.seq,
        clean(&song.title),
        clean(&song.artist),
        clean(&song.album),
        clients,
        u8::from(stream_down),
        tick
    );
    // Written next to the target and renamed, so the game never reads half a line. A failed rename (the game has
    // the file open at that very moment) is simply tried again half a second later.
    let tmp = dir.join("state.tmp");
    if fs::write(&tmp, line).is_ok() {
        let _ = fs::rename(&tmp, dir.join("state.txt"));
    }
}

// ---------------------------------------------------------------- game -> app (cmd.txt)

/// `cmd.txt` holds `<anything>:<anything>:<command>`; a new content means a new command.
fn spawn_command_watcher(dir: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    let _ = thread::Builder::new().name("ytm-cmd".into()).spawn(move || {
        let path = dir.join("cmd.txt");
        // Whatever was left over from an earlier session is not a command.
        let mut last = fs::read_to_string(&path).unwrap_or_default();
        loop {
            thread::sleep(Duration::from_millis(200));
            let Ok(text) = fs::read_to_string(&path) else { continue };
            if text == last {
                continue;
            }
            let name = text.trim().splitn(3, ':').nth(2).unwrap_or("");
            // An unknown or half-written command is ignored and read again on the next round.
            if !COMMANDS.contains(&name) {
                continue;
            }
            last = text.clone();
            log::info!("command from the game: {name}");
            let _ = proxy.send_event(UserEvent::Eval(format!("window.__ytmCmd && window.__ytmCmd('{name}')")));
        }
    });
}

// ---------------------------------------------------------------- main

fn main() {
    let dir = exe_dir();
    init_logger(&dir);
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    log::info!("YTM Radio starting");

    let (pcm, addr) = stream::start(STREAM_ADDR);
    let stream_down = addr.is_none();
    match addr {
        Some(a) => log::info!("stream up: http://{a}/stream.mp3"),
        None => log::error!("stream is NOT up (port {} busy: is YTM Radio already running?)", STREAM_ADDR.1),
    }

    let song = Arc::new(Mutex::new(Song::default()));
    {
        let (dir, song, pcm) = (dir.clone(), song.clone(), pcm.clone());
        let _ = thread::Builder::new().name("ytm-state".into()).spawn(move || {
            let mut tick = 0u64;
            loop {
                tick += 1;
                let current = lock(&song).clone();
                write_state(&dir, &current, pcm.clients(), stream_down, tick);
                thread::sleep(Duration::from_millis(500));
            }
        });
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    spawn_command_watcher(dir.clone(), event_loop.create_proxy());

    let window = WindowBuilder::new()
        .with_title("YTM Radio (YouTube Music)")
        .with_inner_size(LogicalSize::new(520.0, 780.0))
        .build(&event_loop)
        .expect("cannot create the window");

    // The browser profile (your YouTube Music login) is kept here between runs.
    let profile = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.clone())
        .join("YtmRadio")
        .join("webview");
    let mut web_context = WebContext::new(Some(profile));

    let (ipc_pcm, ipc_song) = (pcm.clone(), song.clone());
    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url(START_URL)
        .with_initialization_script(INJECT)
        .with_ipc_handler(move |req| handle_ipc(req.body(), &ipc_pcm, &ipc_song))
        .build(&window)
        .expect("cannot start the web view (is the Microsoft Edge WebView2 runtime installed?)");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _alive = (&webview, &web_context);
        match event {
            Event::UserEvent(UserEvent::Eval(js)) => {
                if let Err(e) = webview.evaluate_script(&js) {
                    log::warn!("could not run a command in the page: {e}");
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                log::info!("window closed, exiting");
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
