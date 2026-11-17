import { TOTAL_DURATION } from '../data/script';

function midiToHz(midi: number): number {
  return 440 * Math.pow(2, (midi - 69) / 12);
}

function encodeWavStereo(buf: AudioBuffer): ArrayBuffer {
  const numChannels = 2;
  const length = buf.length;
  const sampleRate = buf.sampleRate;
  const bytesPerSample = 2;
  const dataBytes = length * numChannels * bytesPerSample;
  const out = new ArrayBuffer(44 + dataBytes);
  const view = new DataView(out);

  const writeStr = (off: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(off + i, s.charCodeAt(i));
  };

  writeStr(0, 'RIFF');
  view.setUint32(4, 36 + dataBytes, true);
  writeStr(8, 'WAVE');
  writeStr(12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, numChannels, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * numChannels * bytesPerSample, true);
  view.setUint16(32, numChannels * bytesPerSample, true);
  view.setUint16(34, 16, true);
  writeStr(36, 'data');
  view.setUint32(40, dataBytes, true);

  const chL = buf.getChannelData(0);
  const chR = buf.numberOfChannels > 1 ? buf.getChannelData(1) : chL;
  let offset = 44;
  for (let i = 0; i < length; i++) {
    const l = Math.max(-1, Math.min(1, chL[i]));
    const r = Math.max(-1, Math.min(1, chR[i]));
    view.setInt16(offset, l < 0 ? l * 0x8000 : l * 0x7fff, true);
    view.setInt16(offset + 2, r < 0 ? r * 0x8000 : r * 0x7fff, true);
    offset += 4;
  }
  return out;
}

function schedulePluck(ctx: BaseAudioContext, dest: AudioNode, time: number, freq: number, gainPeak: number, dur = 0.42): void {
  const osc = ctx.createOscillator();
  const sub = ctx.createOscillator();
  const filter = ctx.createBiquadFilter();
  const gain = ctx.createGain();

  osc.type = 'triangle';
  osc.frequency.setValueAtTime(freq, time);
  sub.type = 'sine';
  sub.frequency.setValueAtTime(freq * 2, time);

  filter.type = 'lowpass';
  filter.frequency.setValueAtTime(Math.min(5200, freq * 6), time);
  filter.frequency.exponentialRampToValueAtTime(Math.max(420, freq * 1.4), time + dur);

  gain.gain.setValueAtTime(0.0001, time);
  gain.gain.linearRampToValueAtTime(gainPeak, time + 0.012);
  gain.gain.exponentialRampToValueAtTime(0.0001, time + dur);

  osc.connect(filter);
  sub.connect(filter);
  filter.connect(gain);
  gain.connect(dest);

  osc.start(time);
  sub.start(time);
  osc.stop(time + dur + 0.03);
  sub.stop(time + dur + 0.03);
}

function scheduleKick(ctx: BaseAudioContext, dest: AudioNode, time: number, peak = 0.24): void {
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.type = 'sine';
  osc.frequency.setValueAtTime(124, time);
  osc.frequency.exponentialRampToValueAtTime(42, time + 0.14);

  gain.gain.setValueAtTime(0.001, time);
  gain.gain.linearRampToValueAtTime(peak, time + 0.008);
  gain.gain.exponentialRampToValueAtTime(0.0001, time + 0.18);

  osc.connect(gain);
  gain.connect(dest);
  osc.start(time);
  osc.stop(time + 0.2);
}

function scheduleBeatClick(ctx: BaseAudioContext, dest: AudioNode, time: number, accent = false): void {
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.type = 'triangle';
  osc.frequency.setValueAtTime(accent ? 1760 : 1180, time);
  gain.gain.setValueAtTime(0.0001, time);
  gain.gain.linearRampToValueAtTime(accent ? 0.05 : 0.025, time + 0.003);
  gain.gain.exponentialRampToValueAtTime(0.0001, time + 0.045);
  osc.connect(gain);
  gain.connect(dest);
  osc.start(time);
  osc.stop(time + 0.06);
}

function scheduleChimeHit(ctx: BaseAudioContext, dest: AudioNode, time: number, notes: number[], peak = 0.09): void {
  notes.forEach((m, idx) => {
    schedulePluck(ctx, dest, time + idx * 0.035, midiToHz(m), peak, 1.35);
  });
}

/**
 * 20s pure commercial electronic + acoustic pluck track (NO TTS).
 */
export class CommercialSynthEngine {
  ctx: AudioContext | null = null;
  enabled = false;
  private masterGain: GainNode | null = null;
  private playingStartedAt = -1;

  async toggle(): Promise<boolean> {
    if (!this.ctx) {
      this.ctx = new AudioContext();
      this.masterGain = this.ctx.createGain();
      this.masterGain.gain.value = 0.0;
      this.masterGain.connect(this.ctx.destination);
    }
    if (this.ctx.state === 'suspended') {
      await this.ctx.resume();
    }
    this.enabled = !this.enabled;
    if (this.masterGain) {
      this.masterGain.gain.setTargetAtTime(this.enabled ? 0.8 : 0.0, this.ctx.currentTime, 0.04);
    }
    return this.enabled;
  }

  previewBurst(): void {
    if (!this.enabled || !this.ctx || !this.masterGain) return;
    if (this.playingStartedAt >= 0 && this.ctx.currentTime - this.playingStartedAt < 0.25) return;
    this.playingStartedAt = this.ctx.currentTime;
    schedulePluck(this.ctx, this.masterGain, this.ctx.currentTime + 0.01, midiToHz(72), 0.14, 0.45);
  }

  async renderOfflineMixWav(durationSec = TOTAL_DURATION, sampleRate = 44100): Promise<ArrayBuffer> {
    const totalSamples = Math.ceil(durationSec * sampleRate);
    const offline = new OfflineAudioContext(2, totalSamples, sampleRate);

    const comp = offline.createDynamicsCompressor();
    comp.threshold.value = -12;
    comp.ratio.value = 3;
    comp.connect(offline.destination);

    const master = offline.createGain();
    master.gain.value = 0.88;
    master.connect(comp);

    // 4-bar uplifting tech chord progression: Fmaj7 - G6 - Em7 - Am9 (128 BPM -> 0.46875s / beat)
    const beat = 60 / 128; // 0.46875s
    const chords = [
      { bass: 41, arp: [60, 64, 67, 72] },
      { bass: 43, arp: [59, 62, 67, 74] },
      { bass: 40, arp: [59, 64, 67, 71] },
      { bass: 45, arp: [60, 64, 67, 76] },
    ];

    // Warm sub bass & sparkly tech arpeggio across 20s
    let step = 0;
    for (let t = 0.02; t < durationSec - 0.6; t += beat * 0.5, step++) {
      const chordIdx = Math.floor(step / 8) % chords.length;
      const chord = chords[chordIdx];

      // Pluck arpeggio 8th-note groove
      const note = chord.arp[step % chord.arp.length] + (step % 16 === 15 ? 12 : 0);
      const duckFade = t >= 19.0 ? Math.max(0.15, (durationSec - t) / 1.0) : 1.0;
      schedulePluck(offline, master, t, midiToHz(note), 0.105 * duckFade, 0.36);

      // Bass + soft kick on downbeats
      if (step % 2 === 0) {
        scheduleKick(offline, master, t, (step % 4 === 0 ? 0.24 : 0.14) * duckFade);
      }
      if (step % 8 === 0) {
        schedulePluck(offline, master, t, midiToHz(chord.bass), 0.15 * duckFade, 1.1);
      }
      scheduleBeatClick(offline, master, t, step % 4 === 0);
    }

    // Precision UI transition hits matching Promo Beat Cuts
    const beatMarkers = [0.02, 2.5, 5.8, 8.6, 11.4, 14.5];
    beatMarkers.forEach((mt, i) => {
      scheduleChimeHit(offline, master, mt, i === 0 ? [60, 64, 67, 72, 79] : [64, 67, 71, 76], 0.085);
    });

    // Warm final Cmaj9 resolution chord at 19.1s
    scheduleChimeHit(offline, master, 19.05, [48, 60, 64, 67, 71, 74], 0.12);

    // Master fade-out last 0.45s so no click at EOF
    master.gain.setValueAtTime(0.88, Math.max(0, durationSec - 0.45));
    master.gain.linearRampToValueAtTime(0.0, durationSec);

    const rendered = await offline.startRendering();
    return encodeWavStereo(rendered);
  }
}
