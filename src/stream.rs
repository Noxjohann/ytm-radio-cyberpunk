//! Live MP3 stream: PCM in, MP3 over HTTP out. Emits silence whenever no audio is queued, so
//! the endpoint is playable from the moment the app starts.
//! Taken from Spot On Radio by Sz4bcsi (SZ4), see LICENSE-SpotOnRadio.txt. Changes: the Spotify / App Capture
//! source switch is removed; the page audio arrives through `push_live`.
//!
//! Encoder API: shine-rs 0.1.5 `Mp3Encoder::encode_interleaved` (src/mp3_encoder.rs). It buffers
//! input and returns one MP3 frame per `samples_per_frame()` interleaved samples.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use shine_rs::{Mp3Encoder, Mp3EncoderConfig, StereoMode};

pub const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: usize = 2;
const BITRATE_KBPS: u32 = 192;
/// ~0.5 s of interleaved PCM. Writers block above this, which paces librespot to real time.
const MAX_BUFFERED: usize = SAMPLE_RATE as usize * CHANNELS / 2;
/// Frames a client may lag behind (~1.7 s) before it is dropped.
const CLIENT_QUEUE: usize = 64;
/// Encoded silence sent to every new client before live audio: 32 frames = ~0.84 s = ~20 KB, more
/// than FMOD's 16 KB stream buffer. RadioExt calls `Channel::setPosition(0)` right after
/// `playSound` on every stream start (RadioExt src/main.cpp `checkSoundLoad`), which replays the
/// first ~0.5 s FMOD buffered; with a silent lead-in the replayed part is silence.
const PREROLL_FRAMES: usize = 32;

/// Every new listener (RadioExt restarts the stream on tune-in and after every menu) hears the music
/// fade in from silence over this long, quadratically: whatever glitch FMOD produces in the first
/// seconds (RadioExt seeks the fresh stream back to 0) then happens at very low volume.
const FADE_SECS: f32 = 2.0;
/// Frame length in samples per channel (MPEG-1 Layer III) and the frame count of the fade (77 ~ 2 s).
const FRAME_SAMPLES: f32 = 1152.0;
const FADE_FRAMES: u32 = 77;

/// Gain (0..=1) of the `frame`-th frame after a listener connected.
fn fade_gain(frame: u32) -> f32 {
    let x = (frame as f32 * FRAME_SAMPLES / SAMPLE_RATE as f32 / FADE_SECS).min(1.0);
    x * x
}

type Frame = Arc<[u8]>;
type Clients = Arc<Mutex<Vec<SyncSender<Frame>>>>;

/// Length in bytes of the MPEG-1 Layer III frame whose header starts at `h`.
fn frame_len(h: &[u8]) -> Option<usize> {
    if h.len() < 4 || h[0] != 0xFF || h[1] & 0xFE != 0xFA {
        return None;
    }
    const KBPS: [usize; 16] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0];
    let kbps = KBPS[usize::from(h[2] >> 4)];
    let rate = *[44_100, 48_000, 32_000].get(usize::from((h[2] >> 2) & 3))?;
    (kbps != 0).then(|| 144 * kbps * 1000 / rate + usize::from((h[2] >> 1) & 1))
}

/// shine-rs returns chunks that are NOT aligned to frame boundaries (a chunk starts with the
/// stuffing bytes that end the previous frame). Clients join at arbitrary moments, so re-cut the
/// byte stream into whole frames: every client then starts exactly on a frame header.
/// (shine keeps its bit reservoir off, `resv_max = 0`, so frames are self-contained.)
#[derive(Default)]
struct Framer {
    carry: Vec<u8>,
}

impl Framer {
    fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.carry.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            // Skip anything before the next frame header (also resyncs after an encoder hiccup);
            // keep the last 3 bytes, a header may be split across two chunks.
            let skip = (0..self.carry.len())
                .find(|&i| frame_len(&self.carry[i..]).is_some())
                .unwrap_or(self.carry.len().saturating_sub(3));
            self.carry.drain(..skip);
            match frame_len(&self.carry) {
                Some(len) if self.carry.len() >= len => out.push(self.carry.drain(..len).collect()),
                _ => return out,
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Interleaved 16-bit stereo PCM queue between the Spotify sink and the encoder clock.
#[derive(Default)]
pub struct Pcm {
    buf: Mutex<VecDeque<i16>>,
    drained: Condvar,
    /// Frames encoded since the last listener connected (saturates at `FADE_FRAMES`).
    fade_frames: AtomicU32,
    /// Listeners currently connected (RadioExt's FMOD channel counts as one).
    clients: AtomicUsize,
}

impl Pcm {
    /// Queue samples; blocks while more than ~0.5 s is already waiting. (Only the tests use it.)
    #[allow(dead_code)]
    pub fn push(&self, samples: &[i16]) {
        let mut buf = lock(&self.buf);
        while buf.len() > MAX_BUFFERED {
            buf = self.drained.wait(buf).unwrap_or_else(PoisonError::into_inner);
        }
        buf.extend(samples);
    }

    /// Queue samples from a live capture (whole stereo frames). Never blocks (the sound card sets the pace, not us):
    /// if the queue is over ~0.5 s the oldest samples are dropped. Both sizes are even, so L/R pairs stay together.
    // ponytail: capture clock and encoder clock drift apart, so a long session drops or pads a few ms now and then.
    pub fn push_live(&self, samples: &[i16]) {
        let mut buf = lock(&self.buf);
        buf.extend(samples);
        let excess = buf.len().saturating_sub(MAX_BUFFERED);
        if excess > 0 {
            buf.drain(..excess);
        }
    }

    /// Number of listeners currently connected to the stream.
    pub fn clients(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }

    /// Start the fade-in over: call when a new listener connects.
    fn restart_fade(&self) {
        self.fade_frames.store(0, Ordering::Relaxed);
    }

    /// Fill `out` from the queue, padding with silence when it runs dry; applies the fade-in.
    fn pop_frame(&self, out: &mut [i16]) {
        {
            let mut buf = lock(&self.buf);
            let n = out.len().min(buf.len());
            for (o, s) in out.iter_mut().zip(buf.drain(..n)) {
                *o = s;
            }
            out[n..].fill(0);
            self.drained.notify_all();
        }
        // fetch_update: a restart between load and store cannot be lost.
        let step = |n: u32| (n < FADE_FRAMES).then_some(n + 1);
        if let Ok(frame) = self.fade_frames.fetch_update(Ordering::Relaxed, Ordering::Relaxed, step) {
            let gain = fade_gain(frame);
            out.iter_mut().for_each(|s| *s = (f32::from(*s) * gain) as i16);
        }
    }
}

/// Start the encoder clock and the HTTP server. Returns the PCM queue and the bound address
/// (`None` if the port could not be bound; the encoder still runs so playback never blocks).
pub fn start(addr: impl ToSocketAddrs) -> (Arc<Pcm>, Option<SocketAddr>) {
    let pcm = Arc::new(Pcm::default());
    let clients = Clients::default();

    let (p, c) = (pcm.clone(), clients.clone());
    spawn("sor-mp3", move || {
        // A panic inside the encoder must not end the stream: log (panic hook) and restart it.
        while catch_unwind(AssertUnwindSafe(|| run_clock(&p, &c))).is_err() {
            log::error!("mp3 encoder panicked; restarting");
            thread::sleep(Duration::from_secs(1));
        }
    });

    let preroll = make_preroll();
    let bound = match TcpListener::bind(addr) {
        Ok(listener) => {
            let bound = listener.local_addr().ok();
            let pcm = pcm.clone();
            spawn("sor-http", move || accept_loop(&listener, &clients, &preroll, &pcm));
            bound
        }
        Err(e) => {
            log::error!("cannot bind the stream port: {e}");
            None
        }
    };
    (pcm, bound)
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = thread::Builder::new().name(name.into()).spawn(f) {
        log::error!("cannot start thread {name}: {e}");
    }
}

fn new_encoder() -> Result<Mp3Encoder, shine_rs::EncoderError> {
    Mp3Encoder::new(
        Mp3EncoderConfig::new()
            .sample_rate(SAMPLE_RATE)
            .bitrate(BITRATE_KBPS)
            .channels(CHANNELS as u8)
            .stereo_mode(StereoMode::Stereo),
    )
}

/// `PREROLL_FRAMES` of encoded silence (empty if the encoder cannot be created).
fn make_preroll() -> Arc<[u8]> {
    let mut enc = match new_encoder() {
        Ok(enc) => enc,
        Err(e) => {
            log::error!("cannot create mp3 encoder for the preroll: {e}");
            return Arc::from(Vec::new());
        }
    };
    let silence = vec![0i16; enc.samples_per_frame()];
    let (mut framer, mut frames) = (Framer::default(), Vec::<Vec<u8>>::new());
    // The encoder holds back its first frames: keep feeding (bounded) and use the last ones.
    for _ in 0..PREROLL_FRAMES * 2 {
        match enc.encode_interleaved(&silence) {
            Ok(out) => out.iter().for_each(|chunk| frames.extend(framer.push(chunk))),
            Err(e) => {
                log::warn!("preroll encode failed: {e}");
                break;
            }
        }
        if frames.len() >= PREROLL_FRAMES + 2 {
            break;
        }
    }
    let from = frames.len().saturating_sub(PREROLL_FRAMES);
    frames[from..].concat().into()
}

fn run_clock(pcm: &Pcm, clients: &Clients) {
    let mut enc = match new_encoder() {
        Ok(enc) => enc,
        Err(e) => return log::error!("cannot create mp3 encoder: {e}"),
    };
    let mut pcm_frame = vec![0i16; enc.samples_per_frame()];
    let tick = Duration::from_nanos(
        (pcm_frame.len() / CHANNELS) as u64 * 1_000_000_000 / u64::from(SAMPLE_RATE),
    );

    let mut framer = Framer::default();
    // Absolute schedule: sleep overshoot (15 ms timer granularity on Windows) self-corrects.
    let mut next = Instant::now();
    loop {
        next += tick;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else if now - next > Duration::from_secs(1) {
            next = now; // e.g. system suspend: don't burst-encode the backlog
        }

        pcm.pop_frame(&mut pcm_frame);
        match enc.encode_interleaved(&pcm_frame) {
            Ok(chunks) => chunks
                .iter()
                .flat_map(|chunk| framer.push(chunk))
                .for_each(|frame| broadcast(clients, frame.into())),
            Err(e) => log::warn!("mp3 encode failed: {e}"),
        }
    }
}

/// Send to every client; drop the ones that are gone or too slow.
fn broadcast(clients: &Clients, frame: Frame) {
    lock(clients).retain(|tx| match tx.try_send(frame.clone()) {
        Ok(()) => true,
        Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
    });
}

fn accept_loop(listener: &TcpListener, clients: &Clients, preroll: &Arc<[u8]>, pcm: &Arc<Pcm>) {
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let (clients, preroll, pcm) = (clients.clone(), preroll.clone(), pcm.clone());
        spawn("sor-client", move || {
            // Connection-level failures (client hung up) are routine, not errors.
            if catch_unwind(AssertUnwindSafe(|| serve_client(conn, &clients, &preroll, &pcm))).is_err() {
                log::error!("stream client thread panicked");
            }
        });
    }
}

/// Keeps the listener count right on every exit path of `serve_client`.
struct Counted<'a>(&'a AtomicUsize);

impl<'a> Counted<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn serve_client(mut conn: TcpStream, clients: &Clients, preroll: &[u8], pcm: &Pcm) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = conn.set_nodelay(true);
    let peer = conn.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut reader = BufReader::new(&conn);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Only the headers that tell us how the player (FMOD in game) behaves are kept, for the log.
    let (mut agent, mut range, mut icy) = (String::new(), String::new(), String::new());
    let mut line = String::new();
    for _ in 0..64 {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(n) if n > 2 => {
                if let Some((key, value)) = line.split_once(':') {
                    let value = value.trim().to_owned();
                    match key.trim().to_ascii_lowercase().as_str() {
                        "user-agent" => agent = value,
                        "range" => range = value,
                        "icy-metadata" => icy = value,
                        _ => {}
                    }
                }
            }
            _ => break,
        }
    }
    drop(reader);

    let mut parts = request_line.split_whitespace();
    let path = parts.nth(1).unwrap_or("").split('?').next().unwrap_or("");
    if path != "/stream.mp3" {
        let _ = conn.write_all(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    log::info!(
        "stream client {peer} connected: {} | user-agent={agent:?} range={range:?} icy-metadata={icy:?}",
        request_line.trim()
    );

    let (tx, rx): (SyncSender<Frame>, Receiver<Frame>) = sync_channel(CLIENT_QUEUE);
    // Restart the fade BEFORE registering: the first frame this client receives is then already faded.
    pcm.restart_fade();
    lock(clients).push(tx);
    let _counted = Counted::new(&pcm.clients);
    let _ = conn.set_write_timeout(Some(Duration::from_secs(10)));
    // Same header set as an Icecast server (what RadioExt's web streams are normally served by).
    // No `icy-metaint`: RadioExt ignores stream tags.
    let head = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\nicy-name: YTM Radio\r\nicy-br: {BITRATE_KBPS}\r\n\
         icy-pub: 0\r\nCache-Control: no-cache\r\nPragma: no-cache\r\nExpires: Mon, 26 Jul 1997 05:00:00 GMT\r\n\
         Connection: close\r\n\r\n"
    );
    // Headers, then the silent lead-in, then live frames (already queued for us since `push` above).
    if conn.write_all(head.as_bytes()).is_err() || conn.write_all(preroll).is_err() {
        return;
    }
    let (started, mut sent) = (Instant::now(), head.len() + preroll.len());
    // recv() errors when the clock drops us (slow client); the write errors when the client leaves.
    while let Ok(frame) = rx.recv() {
        if conn.write_all(&frame).is_err() {
            break;
        }
        sent += frame.len();
    }
    log::info!("stream client {peer} left after {:.1}s, {} KB sent", started.elapsed().as_secs_f32(), sent / 1000);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn get(addr: SocketAddr, path: &str, min_len: usize) -> Vec<u8> {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\nIcy-MetaData: 1\r\n\r\n").unwrap();
        let mut out = Vec::new();
        let mut chunk = [0u8; 2048];
        // Read until the header end plus a few body bytes (stream is endless), or EOF.
        while !out.windows(4).any(|w| w == b"\r\n\r\n") || out.len() < min_len {
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
            }
        }
        out
    }

    fn distinct_bytes(b: &[u8]) -> usize {
        b.iter().collect::<std::collections::HashSet<_>>().len()
    }

    #[test]
    fn fade_curve_starts_silent_ends_full_and_never_dips() {
        assert_eq!(fade_gain(0), 0.0);
        assert!(fade_gain(19) < 0.1, "0.5 s in: {}", fade_gain(19)); // ~0.5 s
        assert_eq!(fade_gain(FADE_FRAMES), 1.0);
        assert!((0..FADE_FRAMES).all(|n| fade_gain(n) <= fade_gain(n + 1)), "must be monotonic");
    }

    #[test]
    fn pop_frame_fades_in_and_a_new_listener_fades_in_again() {
        let pcm = Pcm::default();
        let mut out = vec![0i16; 2304];
        let next = |pcm: &Pcm, out: &mut Vec<i16>| {
            pcm.push(&vec![20_000i16; 2304]);
            pcm.pop_frame(out);
        };
        for _ in 0..FADE_FRAMES + 2 {
            next(&pcm, &mut out);
        }
        assert_eq!(out[0], 20_000, "full level after the fade");

        pcm.restart_fade();
        next(&pcm, &mut out);
        assert!(out.iter().all(|&s| s == 0), "first frame of a new listener must be silent");
        for _ in 0..30 {
            next(&pcm, &mut out);
        }
        assert!(out[0] > 500 && out[0] < 19_000, "mid fade: {}", out[0]);
        for _ in 0..FADE_FRAMES {
            next(&pcm, &mut out);
        }
        assert_eq!(out[0], 20_000, "back to full level");
    }

    #[test]
    fn push_live_never_blocks_and_keeps_the_newest_samples_in_pairs() {
        let pcm = Pcm::default();
        let ramp: Vec<i16> = (0..MAX_BUFFERED as i32 * 2).map(|i| (i % 30_000) as i16).collect();
        pcm.push_live(&ramp); // twice the cap: a blocking push would hang here
        let buf = lock(&pcm.buf);
        assert_eq!(buf.len(), MAX_BUFFERED);
        assert_eq!(buf.len() % 2, 0);
        assert_eq!(*buf.back().unwrap(), *ramp.last().unwrap(), "newest sample kept");
        assert_eq!(*buf.front().unwrap(), ramp[ramp.len() - MAX_BUFFERED], "oldest dropped, pairs intact");
    }

    #[test]
    fn listener_count_follows_connections() {
        let (pcm, addr) = start(("127.0.0.1", 0));
        assert_eq!(pcm.clients(), 0);
        let mut conn = TcpStream::connect(addr.expect("bind")).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(conn, "GET /stream.mp3 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut buf = [0u8; 1024];
        conn.read_exact(&mut buf).unwrap(); // header + preroll: the client is registered by now
        assert_eq!(pcm.clients(), 1);
        drop(conn);
        for _ in 0..40 {
            if pcm.clients() == 0 {
                return;
            }
            thread::sleep(Duration::from_millis(50)); // the writer notices the hang-up within a frame or two
        }
        panic!("listener count never dropped back to 0: {}", pcm.clients());
    }

    #[test]
    fn connecting_restarts_the_fade() {
        let (pcm, addr) = start(("127.0.0.1", 0));
        pcm.fade_frames.store(FADE_FRAMES, Ordering::Relaxed); // pretend an earlier fade finished
        let _ = get(addr.expect("bind"), "/stream.mp3", 100);
        assert!(pcm.fade_frames.load(Ordering::Relaxed) < FADE_FRAMES / 2, "a new client must restart the fade");
    }

    #[test]
    fn framer_recuts_misaligned_encoder_output_into_whole_frames() {
        let mut enc = new_encoder().unwrap();
        let tone: Vec<i16> = (0..enc.samples_per_frame()).map(|i| ((i as f32 * 0.05).sin() * 9000.0) as i16).collect();
        let (mut framer, mut all_in, mut frames) = (Framer::default(), 0, Vec::new());
        for _ in 0..40 {
            for chunk in enc.encode_interleaved(&tone).unwrap() {
                all_in += chunk.len();
                frames.extend(framer.push(&chunk));
            }
        }
        assert!(frames.len() >= 38, "got {}", frames.len());
        for f in &frames {
            assert_eq!(frame_len(f), Some(f.len()), "frame does not start on a header / has the wrong length");
        }
        assert_eq!(frames.iter().map(Vec::len).sum::<usize>() + framer.carry.len(), all_in, "bytes were lost");
    }

    #[test]
    fn framer_resyncs_after_garbage_and_split_headers() {
        let mut enc = new_encoder().unwrap();
        let mut framer = Framer::default();
        let silence = vec![0i16; enc.samples_per_frame()];
        let mut stream = vec![0x12, 0xFF, 0xFF, 0x00]; // junk, incl. a false-looking sync
        for _ in 0..6 {
            enc.encode_interleaved(&silence).unwrap().iter().for_each(|c| stream.extend_from_slice(c));
        }
        // Feed in awkward 7-byte pieces so headers are split across pushes.
        let frames: Vec<Vec<u8>> = stream.chunks(7).flat_map(|piece| framer.push(piece)).collect();
        assert!(frames.len() >= 4, "got {}", frames.len());
        assert!(frames.iter().all(|f| frame_len(f) == Some(f.len())));
    }

    #[test]
    fn preroll_is_whole_valid_frames() {
        let preroll = make_preroll();
        let (mut pos, mut frames) = (0, 0);
        while pos < preroll.len() {
            let len = frame_len(&preroll[pos..])
                .unwrap_or_else(|| panic!("no frame at byte {pos}: {:02x?}", &preroll[pos..(pos + 8).min(preroll.len())]));
            pos += len;
            frames += 1;
        }
        assert_eq!(pos, preroll.len(), "a frame is cut off");
        assert_eq!(frames, PREROLL_FRAMES);
        assert!(preroll.len() >= 16_000, "must fill FMOD's 16 KB buffer, got {}", preroll.len());
    }

    #[test]
    fn every_client_starts_on_a_frame_header_whenever_it_joins() {
        // The encoder's chunks are misaligned in a 4-chunk cycle, so join at many different moments:
        // a single connection could land on the one aligned phase by luck.
        let (pcm, addr) = start(("127.0.0.1", 0));
        let addr = addr.expect("bind");
        pcm.push(&vec![1234i16; SAMPLE_RATE as usize]); // non-silent live audio
        let preroll_len = make_preroll().len();
        for attempt in 0..10 {
            thread::sleep(Duration::from_millis(23));
            let res = get(addr, "/stream.mp3", 400 + preroll_len + 1_400); // preroll + >= 2 live frames
            let body = &res[res.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..];
            let mut pos = 0;
            while let Some(len) = frame_len(&body[pos..]) {
                if pos + len > body.len() {
                    break; // the read stopped mid-frame
                }
                pos += len;
            }
            assert!(pos >= preroll_len + 1_200, "attempt {attempt}: frame sync lost at byte {pos} (preroll {preroll_len})");
        }
    }

    #[test]
    fn preroll_comes_first_on_the_wire() {
        let (_pcm, addr) = start(("127.0.0.1", 0));
        let preroll = make_preroll();
        let res = get(addr.expect("bind"), "/stream.mp3", 1_000_000_usize.min(0)); // header only first
        let head_end = res.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let head = String::from_utf8_lossy(&res[..head_end]).to_lowercase();
        for want in ["http/1.0 200", "content-type: audio/mpeg", "icy-name:", "icy-br: 192", "connection: close"] {
            assert!(head.contains(want), "missing {want:?} in {head}");
        }
        // Read exactly header + preroll and compare.
        let res = get(addr.unwrap(), "/stream.mp3", head_end + preroll.len());
        assert_eq!(&res[head_end..head_end + preroll.len()], &preroll[..], "wire bytes differ from the preroll");
    }

    #[test]
    fn busy_port_is_reported_not_fatal() {
        // A second copy of the mod (or another program) already owns the port.
        let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let (pcm, addr) = start(taken.local_addr().unwrap());
        assert!(addr.is_none(), "must report that the stream is not up");
        pcm.push(&[0; 100]); // the encoder clock still runs: Spotify playback is never stuck
    }

    #[test]
    fn serves_mp3() {
        let (pcm, addr) = start(("127.0.0.1", 0));
        let addr = addr.expect("bind");

        // Silence first: nothing pushed yet, the stream must still be valid MP3.
        let res = get(addr, "/stream.mp3", 600);
        let head_end = res.windows(4).position(|w| w == b"\r\n\r\n").expect("header end") + 4;
        let head = String::from_utf8_lossy(&res[..head_end]).to_lowercase();
        assert!(head.starts_with("http/1.0 200"), "{head}");
        assert!(head.contains("content-type: audio/mpeg"), "{head}");
        let body = &res[head_end..];
        // MPEG-1 Layer III frame sync: 0xFF then 0b111_11_01_x.
        assert_eq!(body[0], 0xFF);
        assert_eq!(body[1] & 0xFE, 0xFA, "not an MPEG-1 L3 frame: {:02x}", body[1]);

        // Real audio must reach the stream: encoded silence is near-constant bytes, a sine is not.
        // (Every connection starts with the silent preroll, so measure the bytes after it.)
        let preroll_len = make_preroll().len();
        let silence_distinct = distinct_bytes(&body[..preroll_len.min(body.len())]);
        let sine: Vec<i16> = (0..SAMPLE_RATE as usize * CHANNELS)
            .map(|i| (((i / 2) as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 8000.0) as i16)
            .collect();
        pcm.push(&sine);
        let res = get(addr, "/stream.mp3", preroll_len + 16_000);
        let head_end = res.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let live = &res[head_end + preroll_len..];
        assert!(distinct_bytes(live) > silence_distinct + 40, "sine did not reach the stream");

        assert!(String::from_utf8_lossy(&get(addr, "/nope", 0)).starts_with("HTTP/1.0 404"));
    }
}
