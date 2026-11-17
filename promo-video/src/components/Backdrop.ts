export class BackdropRenderer {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('2D canvas context unavailable for #bg-canvas');
    this.ctx = ctx;
  }

  resize(width: number, height: number): void {
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
  }

  render(t: number, w: number, h: number): void {
    this.resize(w, h);
    const ctx = this.ctx;

    ctx.fillStyle = '#060913';
    ctx.fillRect(0, 0, w, h);

    // Subtle Mica-style drifting color orbs
    const orb1x = w * (0.22 + 0.12 * Math.sin(t * 0.42));
    const orb1y = h * (0.24 + 0.09 * Math.cos(t * 0.35));
    const g1 = ctx.createRadialGradient(orb1x, orb1y, 20, orb1x, orb1y, Math.max(w, h) * 0.55);
    g1.addColorStop(0, 'rgba(34, 211, 238, 0.20)');
    g1.addColorStop(0.5, 'rgba(14, 116, 144, 0.08)');
    g1.addColorStop(1, 'rgba(6, 9, 19, 0)');
    ctx.fillStyle = g1;
    ctx.fillRect(0, 0, w, h);

    const orb2x = w * (0.78 - 0.1 * Math.cos(t * 0.31));
    const orb2y = h * (0.72 + 0.1 * Math.sin(t * 0.39));
    const g2 = ctx.createRadialGradient(orb2x, orb2y, 20, orb2x, orb2y, Math.max(w, h) * 0.58);
    g2.addColorStop(0, 'rgba(251, 114, 153, 0.18)');
    g2.addColorStop(0.5, 'rgba(99, 102, 241, 0.08)');
    g2.addColorStop(1, 'rgba(6, 9, 19, 0)');
    ctx.fillStyle = g2;
    ctx.fillRect(0, 0, w, h);

    // Architectural CAD hairline grid
    const step = 64;
    ctx.strokeStyle = 'rgba(148, 163, 184, 0.055)';
    ctx.lineWidth = 1;
    const offset = (t * 14) % step;
    ctx.beginPath();
    for (let x = offset; x < w; x += step) {
      ctx.moveTo(x, 0);
      ctx.lineTo(x, h);
    }
    for (let y = offset; y < h; y += step) {
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
    }
    ctx.stroke();
  }
}
