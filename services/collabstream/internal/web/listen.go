package web

import "fmt"

// ListenPage returns the full listener HTML page for a stream.
// The public URL is used to construct the WebSocket endpoint.
func ListenPage(streamID, publicURL string) string {
	return fmt.Sprintf(listenPageTemplate, streamID, publicURL)
}

// listenPageTemplate is a standalone HTML listener page.
// %q substitutions: streamID, publicURL
const listenPageTemplate = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>DAWStream – Futureboard Studio</title>
<style>
*{box-sizing:border-box;margin:0;padding:0}
body{background:#0e1117;color:#c9cdd4;font-family:system-ui,sans-serif;font-size:13px;display:flex;flex-direction:column;align-items:center;justify-content:center;min-height:100vh;gap:0}
.shell{width:340px;background:#151b24;border:1px solid rgba(255,255,255,.07);border-radius:8px;overflow:hidden;box-shadow:0 8px 32px rgba(0,0,0,.5)}
.titlebar{background:#1a2130;border-bottom:1px solid rgba(255,255,255,.06);padding:9px 14px;display:flex;align-items:center;gap:8px;height:32px}
.dot{width:10px;height:10px;border-radius:50%}
.dot-r{background:#ff5f57}.dot-y{background:#febc2e}.dot-g{background:#28c840}
.brand{margin-left:auto;font-size:11px;color:#576070;letter-spacing:.04em}
.body{padding:20px}
.title{font-size:15px;font-weight:600;color:#e2e6ed;margin-bottom:4px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.status-row{display:flex;align-items:center;gap:6px;margin-bottom:18px;color:#576070}
.badge{display:inline-flex;align-items:center;gap:5px;font-size:11px;padding:2px 8px;border-radius:3px;font-weight:500;letter-spacing:.03em}
.badge-live{background:rgba(40,200,64,.15);color:#28c840}
.badge-offline{background:rgba(255,255,255,.06);color:#576070}
.badge-wait{background:rgba(254,188,46,.1);color:#febc2e}
.pulse{width:7px;height:7px;border-radius:50%;background:currentColor;animation:pulse 1.2s ease infinite}
@keyframes pulse{0%,100%{opacity:1}50%{opacity:.3}}
.controls{display:flex;align-items:center;gap:10px;margin-bottom:16px}
.play-btn{width:36px;height:36px;border-radius:50%;background:#2563eb;border:none;cursor:pointer;display:flex;align-items:center;justify-content:center;flex-shrink:0;transition:background .15s}
.play-btn:hover{background:#3b82f6}
.play-btn:disabled{background:#1e2a3a;cursor:not-allowed}
.play-icon{width:0;height:0;border-style:solid;border-width:7px 0 7px 13px;border-color:transparent transparent transparent #fff;margin-left:2px}
.pause-icon{display:flex;gap:3px}.pause-bar{width:4px;height:14px;background:#fff;border-radius:1px}
.vol-row{display:flex;align-items:center;gap:8px;flex:1}
.vol-label{color:#576070;min-width:20px}
input[type=range]{-webkit-appearance:none;width:100%;height:3px;background:#2a3447;border-radius:2px;outline:none}
input[type=range]::-webkit-slider-thumb{-webkit-appearance:none;width:12px;height:12px;border-radius:50%;background:#2563eb;cursor:pointer}
.meta{display:flex;gap:16px;font-size:11px;color:#576070;border-top:1px solid rgba(255,255,255,.05);padding-top:12px}
.meta-item{display:flex;flex-direction:column;gap:2px}
.meta-val{color:#8b95a3;font-variant-numeric:tabular-nums}
.footer{margin-top:12px;font-size:11px;color:#3a4456;text-align:center}
.conn{font-size:11px;color:#576070;margin-top:10px;text-align:center;min-height:16px}
</style>
</head>
<body>
<div class="shell">
  <div class="titlebar">
    <div class="dot dot-r"></div>
    <div class="dot dot-y"></div>
    <div class="dot dot-g"></div>
    <span class="brand">DAWStream · Futureboard Studio</span>
  </div>
  <div class="body">
    <div class="title" id="streamTitle">Loading…</div>
    <div class="status-row">
      <span class="badge badge-wait" id="statusBadge"><span class="pulse"></span>Connecting</span>
      <span id="listenerCount" style="margin-left:auto;color:#3a4456"></span>
    </div>
    <div class="controls">
      <button class="play-btn" id="playBtn" disabled title="Play">
        <div class="play-icon" id="playIcon"></div>
      </button>
      <div class="vol-row">
        <span class="vol-label">Vol</span>
        <input type="range" id="volSlider" min="0" max="100" value="80">
      </div>
    </div>
    <div class="meta">
      <div class="meta-item"><span>Codec</span><span class="meta-val" id="metaCodec">—</span></div>
      <div class="meta-item"><span>Rate</span><span class="meta-val" id="metaRate">—</span></div>
      <div class="meta-item"><span>Ch</span><span class="meta-val" id="metaCh">—</span></div>
      <div class="meta-item"><span>Latency</span><span class="meta-val" id="metaLatency">—</span></div>
    </div>
    <div class="conn" id="connMsg">Connecting to stream…</div>
  </div>
</div>
<script>
(function(){
const STREAM_ID = %q;
const PUBLIC_URL = %q;
const WS_URL = (() => {
  const base = PUBLIC_URL.replace(/^https?:\/\//, '');
  const scheme = PUBLIC_URL.startsWith('https') ? 'wss' : 'ws';
  return scheme + '://' + base + '/ws/listen/' + STREAM_ID;
})();

let audioCtx = null;
let gainNode = null;
let playing = false;
let live = false;

// PCM Float32 playback state
let sampleRate = 48000;
let channels = 2;
let frameMs = 20;
let scheduleTime = 0;
const BUFFER_AHEAD_S = 0.15;

const $title = document.getElementById('streamTitle');
const $badge = document.getElementById('statusBadge');
const $count = document.getElementById('listenerCount');
const $play = document.getElementById('playBtn');
const $playIcon = document.getElementById('playIcon');
const $vol = document.getElementById('volSlider');
const $codec = document.getElementById('metaCodec');
const $rate = document.getElementById('metaRate');
const $ch = document.getElementById('metaCh');
const $latency = document.getElementById('metaLatency');
const $conn = document.getElementById('connMsg');

$vol.addEventListener('input', () => {
  if (gainNode) gainNode.gain.value = $vol.value / 100;
});

$play.addEventListener('click', async () => {
  if (!audioCtx) {
    audioCtx = new (window.AudioContext || window.webkitAudioContext)({ sampleRate });
    gainNode = audioCtx.createGain();
    gainNode.gain.value = $vol.value / 100;
    gainNode.connect(audioCtx.destination);
    scheduleTime = audioCtx.currentTime + BUFFER_AHEAD_S;
  }
  if (audioCtx.state === 'suspended') await audioCtx.resume();
  playing = !playing;
  updatePlayBtn();
  $conn.textContent = playing ? 'Playing' : 'Paused';
});

function updatePlayBtn() {
  $play.disabled = !live;
  $playIcon.className = playing ? '' : 'play-icon';
  if (playing) {
    $playIcon.innerHTML = '<div class="pause-bar"></div><div class="pause-bar"></div>';
    $playIcon.style.cssText = 'display:flex;gap:3px';
  } else {
    $playIcon.innerHTML = '';
    $playIcon.style.cssText = '';
    $playIcon.className = 'play-icon';
  }
}

function setBadge(state) {
  const map = {
    live:  ['badge-live','🔴 Live'],
    offline: ['badge-offline','Offline'],
    wait: ['badge-wait','Waiting'],
    connecting: ['badge-wait','Connecting'],
  };
  const [cls, label] = map[state] || map.wait;
  $badge.className = 'badge ' + cls;
  $badge.innerHTML = (state === 'live' ? '<span class="pulse"></span>' : '') + label;
}

function schedulePCMFrame(data) {
  if (!audioCtx || !playing) return;
  const f32 = new Float32Array(data);
  const frameLen = Math.floor(f32.length / channels);
  const buf = audioCtx.createBuffer(channels, frameLen, sampleRate);
  for (let ch = 0; ch < channels; ch++) {
    const out = buf.getChannelData(ch);
    for (let i = 0; i < frameLen; i++) {
      out[i] = f32[i * channels + ch];
    }
  }
  const now = audioCtx.currentTime;
  if (scheduleTime < now) scheduleTime = now + BUFFER_AHEAD_S;
  const src = audioCtx.createBufferSource();
  src.buffer = buf;
  src.connect(gainNode);
  src.start(scheduleTime);
  const before = scheduleTime;
  scheduleTime += buf.duration;
  $latency.textContent = Math.round((before - now) * 1000) + 'ms';
}

let ws = null;
let reconnectDelay = 1000;

function connect() {
  setBadge('connecting');
  $conn.textContent = 'Connecting…';
  ws = new WebSocket(WS_URL);
  ws.binaryType = 'arraybuffer';

  ws.onopen = () => {
    reconnectDelay = 1000;
    $conn.textContent = 'Connected';
  };

  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') {
      try {
        const msg = JSON.parse(ev.data);
        handleControl(msg);
      } catch(e) {}
    } else {
      schedulePCMFrame(ev.data);
    }
  };

  ws.onclose = () => {
    live = false;
    playing = false;
    setBadge('offline');
    updatePlayBtn();
    $conn.textContent = 'Disconnected. Reconnecting in ' + (reconnectDelay/1000).toFixed(1) + 's…';
    setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 1.5, 15000);
  };

  ws.onerror = () => {};
}

function handleControl(msg) {
  switch(msg.type) {
    case 'stream:info':
      $title.textContent = msg.title || 'DAWStream';
      document.title = (msg.title || 'DAWStream') + ' · Futureboard';
      sampleRate = msg.sampleRate || 48000;
      channels = msg.channels || 2;
      frameMs = msg.frameMs || 20;
      $codec.textContent = msg.codec || '—';
      $rate.textContent = (sampleRate/1000).toFixed(1) + ' kHz';
      $ch.textContent = channels === 2 ? 'Stereo' : channels;
      break;
    case 'stream:status':
      live = msg.status === 'live';
      setBadge(live ? 'live' : 'offline');
      $count.textContent = msg.listeners != null ? msg.listeners + ' listener' + (msg.listeners !== 1 ? 's' : '') : '';
      $play.disabled = !live;
      if (!live) { playing = false; updatePlayBtn(); }
      $conn.textContent = live ? 'Stream is live' : 'Stream is offline';
      break;
    case 'stream:error':
      $conn.textContent = 'Error: ' + msg.message;
      break;
  }
}

connect();
})();
</script>
</body>
</html>`
