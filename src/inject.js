// YTM Radio: runs inside the YouTube Music page (injected by the player app at every page load).
// - taps the page's <video> element with the Web Audio API and sends the sound to the app (nothing reaches the speakers)
// - reports the song (title / artist / album / playing) once a second
// - exposes window.__ytmCmd(name) so the app can press next / previous / play-pause
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

  let ctx = null;
  let tapped = null;

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
    const source = ctx.createMediaElementSource(video);
    // ScriptProcessor (deprecated but works everywhere, and needs no extra script file that the page's CSP could block).
    const proc = ctx.createScriptProcessor(4096, 2, 2);
    proc.onaudioprocess = (e) => {
      const input = e.inputBuffer;
      const left = input.getChannelData(0);
      const right = input.numberOfChannels > 1 ? input.getChannelData(1) : left;
      const n = left.length;
      const pcm = new Int16Array(n * 2);
      for (let i = 0; i < n; i++) {
        let l = left[i];
        let r = right[i];
        l = l > 1 ? 1 : l < -1 ? -1 : l;
        r = r > 1 ? 1 : r < -1 ? -1 : r;
        pcm[2 * i] = (l * 32767) | 0;
        pcm[2 * i + 1] = (r * 32767) | 0;
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
    }));
  }, 1000);

  const click = (selector) => {
    const b = document.querySelector(selector);
    if (b) b.click();
    return !!b;
  };

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
      default:
        post('L:unknown command ' + name);
    }
  };
})();
