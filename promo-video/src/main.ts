import { CommercialSynthEngine } from './audio/synth';
import { TOTAL_DURATION } from './data/script';
import { AspectRatio, PromoOrchestrator } from './timeline';

declare global {
  interface Window {
    __OVEROCODER_DURATION: number;
    __seek: (t: number) => void;
    __setRatio: (ratio: AspectRatio) => void;
    __setCoverMode: (on: boolean) => void;
    __renderOfflineAudioMix: (uploadUrl?: string) => Promise<number>;
  }
}

const params = new URLSearchParams(location.search);
const isRenderMode = params.get('render') === '1';
const initialRatio: AspectRatio = params.get('ratio') === '16:9' ? '16:9' : '9:16';
const initialCover = params.get('cover') === '1';

if (isRenderMode) {
  document.body.classList.add('render-mode');
}

const stage = document.getElementById('stage') as HTMLElement;
const wrap = document.getElementById('viewport-wrap') as HTMLElement;
const btnPlay = document.getElementById('btn-play') as HTMLButtonElement;
const btnRestart = document.getElementById('btn-restart') as HTMLButtonElement;
const scrubber = document.getElementById('scrubber') as HTMLInputElement;
const timeReadout = document.getElementById('time-readout') as HTMLElement;
const btnRatio = document.getElementById('btn-ratio') as HTMLButtonElement;
const btnCover = document.getElementById('btn-cover') as HTMLButtonElement;
const btnAudio = document.getElementById('btn-audio') as HTMLButtonElement;
const btnSafe = document.getElementById('btn-safe') as HTMLButtonElement;
const safeZone = document.getElementById('safe-zone-guide') as HTMLElement;

let currentRatio: AspectRatio = initialRatio;
let currentTime = 0;
let isPlaying = false;
let lastRafTs = 0;

const audio = new CommercialSynthEngine();
const orchestrator = new PromoOrchestrator(stage);

function fitStage(): void {
  const sw = currentRatio === '9:16' ? 1080 : 1920;
  const sh = currentRatio === '9:16' ? 1920 : 1080;
  if (isRenderMode) {
    stage.style.transform = 'none';
    return;
  }
  const pad = 24;
  const scale = Math.min(
    Math.max(0.1, (wrap.clientWidth - pad) / sw),
    Math.max(0.1, (wrap.clientHeight - pad) / sh)
  );
  stage.style.transform = `scale(${scale.toFixed(4)})`;
}

function applyRatio(next: AspectRatio): void {
  currentRatio = next;
  stage.dataset.ratio = next;
  document.body.dataset.ratio = next;
  btnRatio.textContent = `比例: ${next}`;
  fitStage();
  orchestrator.seek(currentTime);
}

function setCoverMode(on: boolean): void {
  document.body.classList.toggle('cover-mode', on);
  btnCover.textContent = on ? '🖼 退出封面' : '🖼 封面';
}

function seekTo(t: number): void {
  currentTime = Math.max(0, Math.min(TOTAL_DURATION, t));
  orchestrator.seek(currentTime);
  scrubber.value = currentTime.toFixed(2);
  timeReadout.textContent = `${currentTime.toFixed(2).padStart(5, '0')} / ${TOTAL_DURATION.toFixed(2)}s`;
}

function loop(now: number): void {
  if (!isPlaying) return;
  const dt = Math.min(0.1, Math.max(0, (now - lastRafTs) / 1000));
  lastRafTs = now;
  const next = currentTime + dt;
  if (next >= TOTAL_DURATION) {
    seekTo(TOTAL_DURATION);
    isPlaying = false;
    btnPlay.textContent = '▶ 播放 / 暂停';
    return;
  }
  seekTo(next);
  audio.previewBurst();
  requestAnimationFrame(loop);
}

function setPlaying(next: boolean): void {
  if (next) setCoverMode(false);
  isPlaying = next;
  btnPlay.textContent = isPlaying ? '⏸ 暂停' : '▶ 播放 / 暂停';
  if (isPlaying) {
    if (currentTime >= TOTAL_DURATION - 0.05) seekTo(0);
    lastRafTs = performance.now();
    requestAnimationFrame(loop);
  }
}

window.__OVEROCODER_DURATION = TOTAL_DURATION;
window.__seek = (t: number) => {
  setPlaying(false);
  seekTo(t);
};
window.__setRatio = (r: AspectRatio) => applyRatio(r);
window.__setCoverMode = (on: boolean) => setCoverMode(on);
window.__renderOfflineAudioMix = async (uploadUrl?: string): Promise<number> => {
  const wav = await audio.renderOfflineMixWav(TOTAL_DURATION, 44100);
  if (uploadUrl) {
    const res = await fetch(uploadUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'audio/wav' },
      body: wav,
    });
    if (!res.ok) throw new Error(`WAV upload HTTP ${res.status}`);
  }
  return wav.byteLength;
};

if (!isRenderMode) {
  btnPlay.addEventListener('click', () => setPlaying(!isPlaying));
  btnRestart.addEventListener('click', () => {
    seekTo(0);
    setPlaying(true);
  });
  scrubber.addEventListener('input', () => {
    setPlaying(false);
    seekTo(Number(scrubber.value));
  });
  btnRatio.addEventListener('click', () => applyRatio(currentRatio === '9:16' ? '16:9' : '9:16'));
  btnCover.addEventListener('click', () => setCoverMode(!document.body.classList.contains('cover-mode')));
  btnSafe.addEventListener('click', () => {
    const hidden = safeZone.classList.toggle('hidden');
    btnSafe.textContent = hidden ? '安全区: 关' : '安全区: 开';
  });
  btnAudio.addEventListener('click', async () => {
    const on = await audio.toggle();
    btnAudio.textContent = on ? '🔊 音频: 开' : '🔇 音频: 关';
  });
  window.addEventListener('keydown', (e) => {
    if (e.code === 'Space') {
      e.preventDefault();
      setPlaying(!isPlaying);
    } else if (e.code === 'ArrowRight') {
      setPlaying(false);
      seekTo(currentTime + 0.5);
    } else if (e.code === 'ArrowLeft') {
      setPlaying(false);
      seekTo(currentTime - 0.5);
    }
  });
}

window.addEventListener('resize', fitStage);
applyRatio(initialRatio);
setCoverMode(initialCover);
seekTo(0);
