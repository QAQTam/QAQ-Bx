import gsap from 'gsap';
import { BackdropRenderer } from './components/Backdrop';
import { WinUiStage } from './components/WinUiStage';
import { cueAt, sampleViewportAt, TOTAL_DURATION } from './data/script';

export type AspectRatio = '9:16' | '16:9';

export class PromoOrchestrator {
  readonly master: gsap.core.Timeline;
  private stage: HTMLElement;
  private backdrop: BackdropRenderer;
  private winuiStage: WinUiStage;

  private hudAct: HTMLElement;
  private hudDip: HTMLElement;
  private hudProgress: HTMLElement;
  private captionText: HTMLElement;

  private s2Kicker: HTMLElement;
  private s2Title: HTMLElement;
  private s2Sub: HTMLElement;
  private calloutEl: HTMLElement;
  private rulerFill: HTMLElement;
  private rulerPin: HTMLElement;
  private metricWidth: HTMLElement;
  private metricCols: HTMLElement;
  private metricChrome: HTMLElement;

  constructor(stage: HTMLElement) {
    this.stage = stage;
    this.backdrop = new BackdropRenderer(document.getElementById('bg-canvas') as HTMLCanvasElement);
    this.winuiStage = new WinUiStage(document.getElementById('winui-stage') as HTMLCanvasElement);

    this.hudAct = document.getElementById('hud-act') as HTMLElement;
    this.hudDip = document.getElementById('hud-dip') as HTMLElement;
    this.hudProgress = document.getElementById('hud-progress-fill') as HTMLElement;
    this.captionText = document.getElementById('caption-text') as HTMLElement;

    this.s2Kicker = document.getElementById('s2-kicker') as HTMLElement;
    this.s2Title = document.getElementById('s2-title') as HTMLElement;
    this.s2Sub = document.getElementById('s2-sub') as HTMLElement;
    this.calloutEl = document.getElementById('stage-callout') as HTMLElement;
    this.rulerFill = document.getElementById('ruler-fill') as HTMLElement;
    this.rulerPin = document.getElementById('ruler-pin') as HTMLElement;
    this.metricWidth = document.getElementById('metric-width') as HTMLElement;
    this.metricCols = document.getElementById('metric-cols') as HTMLElement;
    this.metricChrome = document.getElementById('metric-chrome') as HTMLElement;

    this.master = gsap.timeline({ paused: true });
    this.buildTimeline();
    this.seek(0);
  }

  private buildTimeline(): void {
    const tl = this.master;

    gsap.set('#scene-1', { autoAlpha: 1, scale: 1 });
    gsap.set('#scene-2', { autoAlpha: 0, y: 24 });
    gsap.set('#scene-4', { autoAlpha: 0, y: 24 });
    gsap.set('.spec-card', { opacity: 0, y: 18 });
    gsap.set('.s4-outro-lockup', { opacity: 0, scale: 0.94 });

    // Beat 1 (0.0 -> 2.5s): Cover Slam + Mica beam sweep
    tl.fromTo(
      '.s1-card',
      { scale: 1.07, opacity: 0.8 },
      { scale: 1, opacity: 1, duration: 0.55, ease: 'power3.out' },
      0
    );
    tl.fromTo(
      '.s1-sweep',
      { xPercent: -140 },
      { xPercent: 220, duration: 1.4, ease: 'power2.inOut' },
      0.45
    );

    // Crossfade Beat 1 -> Scene 2 live WinUI morph stage (t = 2.35 -> 2.8s)
    tl.to('#scene-1', { autoAlpha: 0, scale: 0.94, duration: 0.35, ease: 'power2.inOut' }, 2.35);
    tl.to('#scene-2', { autoAlpha: 1, y: 0, duration: 0.42, ease: 'power3.out' }, 2.48);

    // Crossfade Scene 2 (after 14.5-15.8s expand-back lands!) -> Finale Scene 4 (t = 16.3s)
    tl.to('#scene-2', { autoAlpha: 0, y: -18, duration: 0.38, ease: 'power2.inOut' }, 16.25);
    tl.to('#scene-4', { autoAlpha: 1, y: 0, duration: 0.42, ease: 'power3.out' }, 16.38);
    tl.to('.spec-card', { opacity: 1, y: 0, stagger: 0.11, duration: 0.4, ease: 'power3.out' }, 16.55);
    tl.to('.s4-outro-lockup', { opacity: 1, scale: 1, duration: 0.55, ease: 'back.out(1.4)' }, 17.25);

    // Pad total duration to exact 20.00s
    tl.to({}, { duration: 0.1 }, TOTAL_DURATION - 0.1);
  }

  seek(t: number): void {
    const clamped = Math.max(0, Math.min(TOTAL_DURATION, t));
    this.master.seek(clamped, false);

    const ratio = this.stage.dataset.ratio === '16:9' ? '16:9' : '9:16';
    const w = ratio === '9:16' ? 1080 : 1920;
    const h = ratio === '9:16' ? 1920 : 1080;

    this.backdrop.render(clamped, w, h);
    this.winuiStage.render(clamped);

    const cue = cueAt(clamped);
    const vp = sampleViewportAt(clamped);

    this.hudAct.textContent = cue.hudAct;
    this.hudDip.textContent = `${vp.w} × ${vp.h} DIP`;
    this.hudProgress.style.width = `${((clamped / TOTAL_DURATION) * 100).toFixed(2)}%`;
    this.captionText.textContent = cue.caption;

    this.s2Kicker.textContent = cue.headerKicker;
    this.s2Title.textContent = cue.headerTitle;
    this.s2Sub.textContent = cue.headerSub;

    if (cue.callout) {
      this.calloutEl.textContent = cue.callout;
      this.calloutEl.style.opacity = '1';
    } else {
      this.calloutEl.style.opacity = '0';
    }

    // Ruler indicator maps 1280 -> 0%, 390 -> 100%
    const pct = Math.max(0, Math.min(100, ((1280 - vp.w) / (1280 - 390)) * 100));
    this.rulerFill.style.width = `${pct.toFixed(1)}%`;
    this.rulerPin.style.left = `${pct.toFixed(1)}%`;

    this.metricWidth.innerHTML = `WIDTH <strong>${vp.w} DIP</strong>`;
    this.metricCols.innerHTML = `GRID <strong>${vp.cols} COLS</strong>`;
    this.metricChrome.innerHTML = `MODE <strong>${vp.chrome}</strong>`;
  }
}
