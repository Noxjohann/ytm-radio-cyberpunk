// YTM Radio: runs inside the YouTube Music page (injected by the player app at every page load).
// - taps the page's <video> element with the Web Audio API and sends the sound to the app (nothing reaches the speakers)
// - evens out the loudness between songs (leveler, on by default, can be switched off from the game)
// - reports the song (title / artist / album / playing / leveler on) once a second
// - exposes window.__ytmCmd(name) so the app can press next / previous / play-pause / start a mix / switch the leveler
// Messages to the app go through window.ipc.postMessage(string):
//   "A:<base64 of interleaved little-endian 16-bit stereo PCM, 44.1 kHz>"   audio
//   "M:<json>"                                                               song info
//   "L:<text>"                                                               log line
(function () {
  'use strict';
  if (window.top !== window.self) return;
  if (!/(^|\.)music\.youtube\.com$/.test(location.hostname)) return;
  if (window.__ytmRadioLoaded) return;
  window.__ytmRadioLoaded = true;

  const post = (s) => {
    try { window.ipc.postMessage(s); } catch (e) { /* app not listening: ignore */ }
  };

  // While a song plays YouTube Music asks "Leave site?" (a beforeunload handler) before the page may reload, which
  // would hold up the mix reload with a dialog nobody sees in the game. This runs before the page's own scripts, so
  // their beforeunload handlers are simply never registered. (The app window closes through the app, not the page.)
  let unloadPatch = 'not applied';
  let unloadBlocked = 0;
  try {
    const realAdd = EventTarget.prototype.addEventListener;
    EventTarget.prototype.addEventListener = function (type, listener, options) {
      if (type === 'beforeunload') { unloadBlocked++; return undefined; }
      return realAdd.call(this, type, listener, options);
    };
    Object.defineProperty(window, 'onbeforeunload', {
      get: () => null,
      set: (v) => { if (v) unloadBlocked++; },
      configurable: true,
    });
    unloadPatch = 'active';
  } catch (e) {
    unloadPatch = 'failed: ' + e; // the dialog may appear, nothing else breaks
  }

  // ------------------------------------------------------------ loudness leveler
  // Songs on YouTube Music differ a lot in loudness. The leveler measures how loud the music is (RMS, followed
  // slowly) and moves one gain towards the level LEVEL_TARGET_RMS, so every song lands at about the same loudness.
  // A soft limiter keeps boosted peaks from clipping. Tune the numbers here.
  const LEVEL_TARGET_RMS = 0.126;  // about -18 dBFS: what every song is brought to
  const LEVEL_MIN_GAIN = 0.35;     // loudest songs are turned down at most this far (about -9 dB)
  const LEVEL_MAX_GAIN = 3.0;      // quietest songs are turned up at most this far (about +9.5 dB)
  const LEVEL_RISE_SECS = 0.5;     // the measured loudness follows a louder song this fast...
  const LEVEL_FALL_SECS = 3;       // ...and a quieter one this slowly (so a short quiet part is not boosted)
  const LEVEL_DOWN_SECS = 0.3;     // the gain goes DOWN this fast (a loud song must not blast)...
  const LEVEL_UP_SECS = 4;         // ...and UP this slowly (no pumping)
  const LEVEL_GATE_RMS = 0.003;    // blocks quieter than this (about -50 dBFS) are gaps: they do not change anything
  const LIMIT_KNEE = 0.9;          // above this level samples are rounded off towards 1.0

  function softLimit(x) {
    const a = Math.abs(x);
    if (a <= LIMIT_KNEE) return x;
    const over = (a - LIMIT_KNEE) / (1 - LIMIT_KNEE);
    const y = LIMIT_KNEE + (1 - LIMIT_KNEE) * Math.tanh(over);
    return x < 0 ? -y : y;
  }

  function makeLeveler(sampleRate) {
    let ms = LEVEL_TARGET_RMS * LEVEL_TARGET_RMS; // smoothed mean square of the music
    let gain = 1;
    return {
      gain: () => gain,
      // Levels one block in place (left / right Float32Array of the same length).
      process(l, r) {
        const n = l.length;
        const dt = n / sampleRate;
        let sum = 0;
        for (let i = 0; i < n; i++) sum += l[i] * l[i] + r[i] * r[i];
        const blockMs = sum / (2 * n);
        if (Math.sqrt(blockMs) > LEVEL_GATE_RMS) {
          const secs = blockMs > ms ? LEVEL_RISE_SECS : LEVEL_FALL_SECS;
          ms += (blockMs - ms) * (1 - Math.exp(-dt / secs));
        }
        const wanted = Math.min(LEVEL_MAX_GAIN, Math.max(LEVEL_MIN_GAIN, LEVEL_TARGET_RMS / Math.sqrt(ms)));
        const tau = wanted < gain ? LEVEL_DOWN_SECS : LEVEL_UP_SECS;
        gain += (wanted - gain) * (1 - Math.exp(-dt / tau));
        for (let i = 0; i < n; i++) {
          l[i] = softLimit(l[i] * gain);
          r[i] = softLimit(r[i] * gain);
        }
      },
    };
  }
  window.__ytmMakeLeveler = makeLeveler; // also used by the offline test

  // The choice is kept in the page's own storage, so it survives restarts and page reloads (default: on).
  let levelOn = true;
  try { levelOn = localStorage.getItem('ytmRadioLevel') !== '0'; } catch (e) { /* storage unavailable: stay on */ }

  function setLevel(on) {
    levelOn = on;
    try { localStorage.setItem('ytmRadioLevel', on ? '1' : '0'); } catch (e) { /* ignore */ }
    post('L:loudness leveling ' + (on ? 'on' : 'off'));
  }

  // ------------------------------------------------------------ audio tap
  let ctx = null;
  let tapped = null;
  let leveler = null;

  function bytesToBase64(bytes) {
    let s = '';
    const CHUNK = 0x8000;
    for (let i = 0; i < bytes.length; i += CHUNK) {
      s += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
    }
    return btoa(s);
  }

  function tap(video) {
    tapped = video;
    ctx = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 44100 });
    leveler = makeLeveler(ctx.sampleRate);
    const source = ctx.createMediaElementSource(video);
    // ScriptProcessor (deprecated but works everywhere, and needs no extra script file that the page's CSP could block).
    const proc = ctx.createScriptProcessor(4096, 2, 2);
    proc.onaudioprocess = (e) => {
      const input = e.inputBuffer;
      const left = input.getChannelData(0);
      const right = input.numberOfChannels > 1 ? input.getChannelData(1) : left;
      // Work on copies: the input buffers belong to the page's audio graph.
      const l = Float32Array.from(left);
      const r = Float32Array.from(right);
      if (levelOn) leveler.process(l, r);
      const n = l.length;
      const pcm = new Int16Array(n * 2);
      for (let i = 0; i < n; i++) {
        let a = l[i];
        let b = r[i];
        a = a > 1 ? 1 : a < -1 ? -1 : a;
        b = b > 1 ? 1 : b < -1 ? -1 : b;
        pcm[2 * i] = (a * 32767) | 0;
        pcm[2 * i + 1] = (b * 32767) | 0;
      }
      post('A:' + bytesToBase64(new Uint8Array(pcm.buffer)));
      // The output buffer is left as zeros: the speakers stay silent.
    };
    source.connect(proc);
    proc.connect(ctx.destination); // Chrome only runs a ScriptProcessor that is connected to the destination
    post('L:tapped the video element, audio context ' + ctx.sampleRate + ' Hz, state ' + ctx.state);
  }

  function resume() {
    if (ctx && ctx.state !== 'running') {
      ctx.resume().catch(() => {});
    }
  }
  ['click', 'keydown', 'pointerdown'].forEach((ev) => document.addEventListener(ev, resume, true));

  // After "start a mix" the page is reloaded; playback is started again once the video element is back.
  const AUTOPLAY_KEY = 'ytmRadioAutoplay';
  const AUTOPLAY_SECS = 20;

  function autoplayAfterReload(video) {
    let started = 0;
    try { started = Number(sessionStorage.getItem(AUTOPLAY_KEY)) || 0; } catch (e) { return; }
    if (!started) return;
    if (Date.now() - started > AUTOPLAY_SECS * 1000 || !video.paused) {
      try { sessionStorage.removeItem(AUTOPLAY_KEY); } catch (e) { /* ignore */ }
      return;
    }
    video.play().catch(() => {});
  }

  setInterval(() => {
    const video = document.querySelector('video');
    if (video && video !== tapped) {
      try {
        tap(video);
      } catch (e) {
        post('L:cannot tap the video element: ' + e);
        tapped = video; // do not retry every second on the same element
      }
    }
    if (video) autoplayAfterReload(video);
    resume();
  }, 1000);

  setInterval(() => {
    const md = navigator.mediaSession && navigator.mediaSession.metadata;
    const v = tapped || document.querySelector('video');
    let art = '';
    if (md && md.artwork && md.artwork.length) art = md.artwork[md.artwork.length - 1].src || '';
    post('M:' + JSON.stringify({
      t: md ? md.title || '' : '',
      a: md ? md.artist || '' : '',
      al: md ? md.album || '' : '',
      art: art,
      p: !!v && !v.paused && !v.ended,
      lv: levelOn,
    }));
  }, 1000);

  const click = (selector) => {
    const b = document.querySelector(selector);
    if (b) b.click();
    return !!b;
  };

  // "Start a mix": the same as "Start radio" on the song that is playing. YouTube Music's song mix is the playlist
  // "RDAMVM" + the video id; opening that watch address starts it. The page reloads, which is why autoplay is
  // remembered (sessionStorage) and played again after the reload.
  const VIDEO_ID = /^[A-Za-z0-9_-]{6,20}$/;
  const MIX_KEY = 'ytmRadioMixAt';
  const MIX_COOLDOWN_SECS = 15;

  // The id of the song that is playing. The address bar only has it ("watch?v=") while the now-playing screen is
  // open, so the player itself is asked first; the address and the queue are fallbacks. Returns { id, how } or null.
  function currentVideoId() {
    const tries = [
      ['player', () => document.getElementById('movie_player').getVideoData().video_id],
      ['address', () => new URLSearchParams(location.search).get('v')],
      ['queue', () => document.querySelector('ytmusic-player-queue-item[selected]').data.videoId],
    ];
    for (const [how, get] of tries) {
      try {
        const id = get();
        if (typeof id === 'string' && VIDEO_ID.test(id)) return { id, how };
      } catch (e) { /* this source is not available on this page: try the next */ }
    }
    return null;
  }

  function startMix() {
    const found = currentVideoId();
    if (!found) {
      post('L:mix: cannot find the id of the song that is playing (page ' + location.pathname + ', player ' +
        (document.getElementById('movie_player') ? 'found' : 'missing') + ')');
      return;
    }
    const id = found.id;

    // A second press must not start the same thing again: ignore it while a mix is still starting (the page is
    // reloading), and when this song is already the start of its own mix. The time is kept in sessionStorage so it
    // survives the reload.
    let lastMix = 0;
    try { lastMix = Number(sessionStorage.getItem(MIX_KEY)) || 0; } catch (e) { /* ignore */ }
    if (Date.now() - lastMix < MIX_COOLDOWN_SECS * 1000) {
      post('L:mix ignored: one was started less than ' + MIX_COOLDOWN_SECS + ' s ago');
      return;
    }
    if (new URLSearchParams(location.search).get('list') === 'RDAMVM' + id) {
      post('L:mix ignored: this song is already the start of its own mix');
      return;
    }
    try { sessionStorage.setItem(MIX_KEY, String(Date.now())); } catch (e) { /* ignore */ }
    post('L:mix: song id taken from the ' + found.how + '; beforeunload patch ' + unloadPatch + ', ' + unloadBlocked +
      ' page handler(s) blocked so far');
    try { sessionStorage.setItem(AUTOPLAY_KEY, String(Date.now())); } catch (e) { /* ignore */ }
    post('L:starting a mix from ' + id);
    location.assign('https://music.youtube.com/watch?v=' + id + '&list=RDAMVM' + id);
  }

  window.__ytmCmd = function (name) {
    const v = document.querySelector('video');
    switch (name) {
      case 'next':
        click('.next-button');
        break;
      case 'prev':
        click('.previous-button');
        break;
      case 'toggle':
        if (!click('#play-pause-button') && v) { if (v.paused) v.play(); else v.pause(); }
        break;
      case 'pause':
        if (v && !v.paused) v.pause();
        break;
      case 'play':
        if (v && v.paused) v.play();
        break;
      case 'mix':
        startMix();
        break;
      case 'levelon':
        setLevel(true);
        break;
      case 'leveloff':
        setLevel(false);
        break;
      default:
        post('L:unknown command ' + name);
    }
  };
})();
