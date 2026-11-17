import { sampleViewportAt } from '../data/script';

interface DemoManifest {
  count: number;
  fps: number;
  duration: number;
}

const CARD_DATA = [
  { title: '用 Rust 重写原生 WinUI 客户端，内存直接砍到百兆以内', author: 'QAQTam', views: '142.8万', dur: '12:48', c1: '#0ea5e9', c2: '#312e81' },
  { title: '1280 → 390 DIP 丝滑自适应：860 断点下的网格坍缩算法', author: 'Overcoder', views: '89.4万', dur: '08:15', c1: '#fb7185', c2: '#4c1d95' },
  { title: 'libmpv + D3D11 SwapChainPanel 零拷贝直出硬件解码教程', author: 'Rustaceans', views: '64.2万', dur: '15:11', c1: '#10b981', c2: '#0f172a' },
  { title: '彻底告别 Chromium！4核8G 轻薄本上的原生桌面应用体验', author: 'WinUI匠人', views: '200.3万', dur: '06:42', c1: '#f59e0b', c2: '#7c2d12' },
  { title: 'B 站 OpenAPI 签名与风控避坑指南 · 游客免登录也能刷推荐', author: '代码考古', views: '51.9万', dur: '18:30', c1: '#38bdf8', c2: '#1e3a8a' },
  { title: '手把手修复竖屏热门「加载更多」按钮裁切重叠 Bug 全记录', author: '胡闹编程', views: '47.6万', dur: '05:24', c1: '#ec4899', c2: '#831843' },
  { title: 'Mica 云母背景 + Fluent Design：WinUI 3 控件树性能优化', author: 'UI实验室', views: '33.1万', dur: '09:50', c1: '#6366f1', c2: '#1e1b4b' },
  { title: '从 Electron 迁移到 Rust + Windows Reactor 的开发日记', author: 'Overcoder', views: '91.0万', dur: '14:07', c1: '#14b8a6', c2: '#134e4a' },
];

const MORE_TILES = ['动态', '离线缓存', '历史记录', '稍后再看', '我的收藏', '设置'];

const DANMAKU_TRACKS = [
  { text: 'Pure Rust + WinUI 3 真丝滑！', y: 0.22, speed: 0.19, offset: 0.1, color: '#ffffff' },
  { text: '4c8T 8G 轻薄本秒开零卡顿', y: 0.42, speed: 0.23, offset: 0.45, color: '#22d3ee' },
  { text: '< 860 DIP 自动变身竖屏模式绝了', y: 0.62, speed: 0.17, offset: 0.72, color: '#fde68a' },
  { text: 'libmpv SwapChainPanel 原生直出', y: 0.80, speed: 0.21, offset: 0.28, color: '#fbcfe8' },
];

function rr(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number): void {
  const rad = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, rad);
}

export class WinUiStage {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private manifest: DemoManifest | null = null;
  private frameCache = new Map<number, HTMLImageElement>();

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('2D canvas context unavailable for #winui-stage');
    this.ctx = ctx;
    void this.tryLoadCapturedFrames();
  }

  private async tryLoadCapturedFrames(): Promise<void> {
    try {
      const res = await fetch('./demo-frames/manifest.json', { cache: 'no-store' });
      if (!res.ok) return;
      const json = (await res.json()) as DemoManifest;
      if (json && json.count > 0) {
        this.manifest = json;
      }
    } catch {
      // No captured desktop clip yet; procedural Canvas fallback stays active.
    }
  }

  private getCapturedFrame(t: number): HTMLImageElement | null {
    if (!this.manifest || this.manifest.count <= 0) return null;
    const localT = Math.max(0, Math.min(this.manifest.duration, t - 1.8));
    const idx = Math.min(this.manifest.count, Math.max(1, Math.round(localT * this.manifest.fps) + 1));
    const cached = this.frameCache.get(idx);
    if (cached) return cached.complete && cached.naturalWidth > 0 ? cached : null;

    const img = new Image();
    img.src = `./demo-frames/f_${String(idx).padStart(4, '0')}.jpg`;
    this.frameCache.set(idx, img);
    return img.complete && img.naturalWidth > 0 ? img : null;
  }

  render(t: number): void {
    const W = this.canvas.width;
    const H = this.canvas.height;
    const ctx = this.ctx;
    ctx.clearRect(0, 0, W, H);

    const vp = sampleViewportAt(t);
    const baseFitW = (W - 44) / 1280;
    const baseFitH = (H - 56) / 800;
    const narrowProgress = Math.max(0, Math.min(1, (1280 - vp.w) / (1280 - 390)));
    const boost = 1 + 0.68 * narrowProgress;
    const rawScale = Math.min(baseFitW, baseFitH) * boost;
    const scale = Math.min(rawScale, (W - 48) / vp.w, (H - 72) / vp.h);
    const winW = Math.max(260, vp.w * scale);
    const winH = Math.max(400, vp.h * scale);
    const wx = (W - winW) / 2;
    const wy = (H - winH) / 2;

    // Outer ambient window halo
    ctx.save();
    ctx.shadowColor = vp.chrome === 'DESKTOP' ? 'rgba(34, 211, 238, 0.35)' : 'rgba(251, 114, 153, 0.40)';
    ctx.shadowBlur = 44;
    ctx.fillStyle = '#0f172a';
    rr(ctx, wx, wy, winW, winH, 16);
    ctx.fill();
    ctx.restore();

    // If real desktop screen recording frames are bundled, composite them inside the stage frame!
    const captured = this.getCapturedFrame(t);
    if (captured) {
      ctx.save();
      rr(ctx, wx, wy, winW, winH, 16);
      ctx.clip();
      // Crop matching centered portion of the 1280x800 desktop capture
      const srcW = (vp.w / 1280) * captured.naturalWidth;
      const srcH = (vp.h / 800) * captured.naturalHeight;
      const sx = (captured.naturalWidth - srcW) / 2;
      const sy = (captured.naturalHeight - srcH) / 2;
      ctx.drawImage(captured, sx, sy, srcW, srcH, wx, wy, winW, winH);
      ctx.restore();
      ctx.strokeStyle = 'rgba(148, 163, 184, 0.45)';
      ctx.lineWidth = 2;
      rr(ctx, wx, wy, winW, winH, 16);
      ctx.stroke();
      return;
    }

    // Crisp WinUI 3 procedural Mica Light UI shell
    ctx.save();
    rr(ctx, wx, wy, winW, winH, 16);
    ctx.clip();

    // Mica soft acrylic background
    const micaGrad = ctx.createLinearGradient(wx, wy, wx + winW, wy + winH);
    micaGrad.addColorStop(0, '#f3f7fc');
    micaGrad.addColorStop(0.55, '#eef3f9');
    micaGrad.addColorStop(1, '#f8f2f7');
    ctx.fillStyle = micaGrad;
    ctx.fillRect(wx, wy, winW, winH);

    const titleH = 38;
    const isMobile = vp.w < 860;
    const isMedium = vp.w >= 600 && vp.w < 860;

    // TitleBar
    ctx.fillStyle = 'rgba(255, 255, 255, 0.72)';
    ctx.fillRect(wx, wy, winW, titleH);
    ctx.fillStyle = '#fb7299';
    rr(ctx, wx + 14, wy + 11, 16, 16, 4);
    ctx.fill();
    ctx.fillStyle = '#0f172a';
    ctx.font = '700 13px "Segoe UI Variable Text", "Microsoft YaHei UI", sans-serif';
    ctx.textBaseline = 'middle';
    ctx.fillText('QAQ-Bx', wx + 38, wy + 19);

    if (!isMobile) {
      // Inline Desktop search box
      const sbW = Math.min(260, winW * 0.34);
      const sbX = wx + (winW - sbW) / 2;
      ctx.fillStyle = '#ffffff';
      rr(ctx, sbX, wy + 7, sbW, 24, 12);
      ctx.fill();
      ctx.strokeStyle = 'rgba(148, 163, 184, 0.35)';
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.fillStyle = '#64748b';
      ctx.font = '11px "Microsoft YaHei UI", sans-serif';
      ctx.fillText('搜索视频 · BV号 / UP主', sbX + 12, wy + 19);
    } else if (isMedium) {
      // Floating search pill + notification bell
      ctx.fillStyle = vp.mode === 'search' ? '#005fb8' : '#ffffff';
      rr(ctx, wx + winW - 118, wy + 7, 30, 24, 12);
      ctx.fill();
      ctx.strokeStyle = 'rgba(148, 163, 184, 0.35)';
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.fillStyle = vp.mode === 'search' ? '#ffffff' : '#334155';
      ctx.font = '700 11px sans-serif';
      ctx.fillText('🔍', wx + winW - 110, wy + 19);
    }

    // Bell + Window caption dots
    ctx.fillStyle = '#475569';
    ctx.font = '12px sans-serif';
    ctx.fillText('🔔', wx + winW - 82, wy + 19);
    const dotColors = ['#94a3b8', '#94a3b8', '#f43f5e'];
    dotColors.forEach((c, idx) => {
      ctx.fillStyle = c;
      ctx.beginPath();
      ctx.arc(wx + winW - 48 + idx * 14, wy + 19, 3.6, 0, Math.PI * 2);
      ctx.fill();
    });

    // Left Navigation Rail on Desktop (>= 860)
    const railW = isMobile ? 0 : 126;
    if (!isMobile) {
      ctx.fillStyle = 'rgba(255, 255, 255, 0.56)';
      ctx.fillRect(wx, wy + titleH, railW, winH - titleH);
      const navItems = ['推荐首页', '热门榜单', '动态关注', '离线缓存', '我的账号'];
      navItems.forEach((label, idx) => {
        const active = vp.mode === 'popular' ? idx === 1 : idx === 0;
        const ny = wy + titleH + 14 + idx * 34;
        if (active) {
          ctx.fillStyle = 'rgba(0, 95, 184, 0.12)';
          rr(ctx, wx + 8, ny, railW - 16, 28, 7);
          ctx.fill();
          ctx.fillStyle = '#005fb8';
          rr(ctx, wx + 8, ny + 6, 3, 16, 2);
          ctx.fill();
        }
        ctx.fillStyle = active ? '#005fb8' : '#334155';
        ctx.font = `${active ? 700 : 500} 12px "Microsoft YaHei UI", sans-serif`;
        ctx.fillText(label, wx + 20, ny + 14);
      });
    }

    const contentX = wx + railW + 12;
    const contentY = wy + titleH + 10;
    const contentW = winW - railW - 24;
    const bottomNavH = isMobile ? 48 : 0;
    const contentH = winH - titleH - bottomNavH - 18;

    const isPlayer = vp.mode === 'player-intro' || vp.mode === 'player-desc' || vp.mode === 'player-comments';

    if (isPlayer) {
      this.drawPlayerScene(ctx, contentX, contentY, contentW, contentH, isMobile, t, vp.mode);
    } else {
      this.drawGridScene(ctx, contentX, contentY, contentW, contentH, vp.cols, vp.mode, t);
    }

    // Medium 640 Floating Search Overlay
    if (vp.mode === 'search' && isMobile) {
      const ovW = Math.min(contentW - 12, 340);
      const ovX = wx + (winW - ovW) / 2;
      const ovY = wy + titleH + 8;
      ctx.save();
      ctx.shadowColor = 'rgba(15, 23, 42, 0.22)';
      ctx.shadowBlur = 18;
      ctx.fillStyle = '#ffffff';
      rr(ctx, ovX, ovY, ovW, 44, 12);
      ctx.fill();
      ctx.restore();
      ctx.strokeStyle = '#005fb8';
      ctx.lineWidth = 1.6;
      rr(ctx, ovX, ovY, ovW, 44, 12);
      ctx.stroke();

      const progress = Math.max(0, Math.min(1, (t - 8.75) / 1.1));
      const fullQuery = 'Rust WinUI';
      const typed = fullQuery.slice(0, Math.floor(fullQuery.length * progress));
      ctx.fillStyle = '#0f172a';
      ctx.font = '600 13px "Cascadia Code", "Microsoft YaHei UI", sans-serif';
      ctx.fillText(typed + (Math.floor(t * 4) % 2 === 0 ? '|' : ''), ovX + 14, ovY + 22);

      ctx.fillStyle = '#fb7299';
      rr(ctx, ovX + ovW - 60, ovY + 8, 50, 28, 8);
      ctx.fill();
      ctx.fillStyle = '#ffffff';
      ctx.font = '700 11px "Microsoft YaHei UI", sans-serif';
      ctx.fillText('搜索', ovX + ovW - 46, ovY + 22);
    }

    // Bottom Navigation Bar & 2x3 More Menu Overlay on Mobile (< 860)
    if (isMobile) {
      const barY = wy + winH - bottomNavH;
      ctx.fillStyle = 'rgba(255, 255, 255, 0.94)';
      ctx.fillRect(wx, barY, winW, bottomNavH);
      ctx.fillStyle = 'rgba(148, 163, 184, 0.28)';
      ctx.fillRect(wx, barY, winW, 1);

      const tabs = ['首页', '热门', '播放', '我的', '更多'];
      const slotW = winW / tabs.length;
      tabs.forEach((tab, idx) => {
        const active =
          (vp.mode === 'home' && idx === 0) ||
          (vp.mode === 'popular' && idx === 1) ||
          (isPlayer && idx === 2) ||
          (vp.mode === 'more' && idx === 4);
        ctx.fillStyle = active ? '#fb7299' : '#475569';
        ctx.font = `${active ? 700 : 500} 11px "Microsoft YaHei UI", sans-serif`;
        ctx.textAlign = 'center';
        ctx.fillText(tab, wx + slotW * (idx + 0.5), barY + 26);
      });
      ctx.textAlign = 'left';

      // 2x3 Grid Flyout above Bottom Bar
      if (vp.mode === 'more') {
        const flyW = winW - 24;
        const flyH = 148;
        const flyX = wx + 12;
        const flyY = barY - flyH - 10;
        ctx.save();
        ctx.shadowColor = 'rgba(15, 23, 42, 0.24)';
        ctx.shadowBlur = 22;
        ctx.fillStyle = 'rgba(255, 255, 255, 0.96)';
        rr(ctx, flyX, flyY, flyW, flyH, 14);
        ctx.fill();
        ctx.restore();
        ctx.strokeStyle = 'rgba(148, 163, 184, 0.35)';
        ctx.lineWidth = 1;
        rr(ctx, flyX, flyY, flyW, flyH, 14);
        ctx.stroke();

        const tileW = (flyW - 36) / 3;
        const tileH = 54;
        MORE_TILES.forEach((name, idx) => {
          const c = idx % 3;
          const r = Math.floor(idx / 3);
          const tx = flyX + 12 + c * (tileW + 6);
          const ty = flyY + 14 + r * (tileH + 10);
          ctx.fillStyle = '#f1f5f9';
          rr(ctx, tx, ty, tileW, tileH, 10);
          ctx.fill();
          ctx.fillStyle = '#0f172a';
          ctx.font = '600 11px "Microsoft YaHei UI", sans-serif';
          ctx.textAlign = 'center';
          ctx.fillText(name, tx + tileW / 2, ty + tileH / 2);
        });
        ctx.textAlign = 'left';
      }
    }

    ctx.restore();

    // Clean Crisp Window Border
    ctx.strokeStyle = vp.chrome === 'DESKTOP' ? 'rgba(56, 189, 248, 0.55)' : 'rgba(251, 114, 153, 0.65)';
    ctx.lineWidth = 2;
    rr(ctx, wx, wy, winW, winH, 16);
    ctx.stroke();
  }

  private drawGridScene(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    w: number,
    h: number,
    cols: number,
    mode: string,
    t: number
  ): void {
    const isPopular = mode === 'popular' || mode === 'more';
    // Header Strip
    ctx.fillStyle = '#0f172a';
    ctx.font = '700 14px "Microsoft YaHei UI", sans-serif';
    ctx.fillText(isPopular ? '🔥 全站热门榜单' : mode === 'search' ? '🔍 搜索：Rust WinUI' : '✨ 为你推荐', x, y + 12);

    const gap = 10;
    const cardW = (w - gap * (cols - 1)) / cols;
    const coverH = cardW * 0.62;
    const cardH = coverH + 46;
    const gridTop = y + 26;
    const rowsVisible = isPopular ? 2 : 3;

    for (let r = 0; r < rowsVisible; r++) {
      for (let c = 0; c < cols; c++) {
        const idx = (r * cols + c) % CARD_DATA.length;
        const item = CARD_DATA[idx];
        const cx = x + c * (cardW + gap);
        const cy = gridTop + r * (cardH + gap);
        if (cy + cardH > y + h - (isPopular ? 48 : 8)) continue;

        ctx.fillStyle = '#ffffff';
        rr(ctx, cx, cy, cardW, cardH, 10);
        ctx.fill();

        // Cover gradient thumbnail
        const cg = ctx.createLinearGradient(cx, cy, cx + cardW, cy + coverH);
        cg.addColorStop(0, item.c1);
        cg.addColorStop(1, item.c2);
        ctx.fillStyle = cg;
        rr(ctx, cx, cy, cardW, coverH, 10);
        ctx.fill();

        // Duration pill
        ctx.fillStyle = 'rgba(2, 6, 23, 0.76)';
        rr(ctx, cx + cardW - 40, cy + coverH - 18, 34, 14, 4);
        ctx.fill();
        ctx.fillStyle = '#f8fafc';
        ctx.font = '600 9px "Cascadia Code", monospace';
        ctx.fillText(item.dur, cx + cardW - 37, cy + coverH - 10);

        // Card title (1 concise line + author)
        ctx.fillStyle = '#0f172a';
        ctx.font = '600 10px "Microsoft YaHei UI", sans-serif';
        const maxChars = Math.max(6, Math.floor(cardW / 12));
        const clipped = item.title.length > maxChars ? `${item.title.slice(0, maxChars - 1)}…` : item.title;
        ctx.fillText(clipped, cx + 8, cy + coverH + 14);

        ctx.fillStyle = '#64748b';
        ctx.font = '10px "Microsoft YaHei UI", sans-serif';
        ctx.fillText(`${item.author} · ${item.views}`, cx + 8, cy + coverH + 30);
      }
    }

    // Highlight fixed "加载更多" footer cleanly placed BELOW cards without clipping/overlap!
    if (isPopular) {
      const btnW = Math.min(132, w * 0.65);
      const btnH = 34;
      const bx = x + (w - btnW) / 2;
      const by = gridTop + rowsVisible * (cardH + gap) + 4;
      if (by + btnH <= y + h) {
        ctx.save();
        const pulse = 0.55 + 0.45 * Math.sin(t * 7);
        ctx.shadowColor = `rgba(16, 185, 129, ${pulse})`;
        ctx.shadowBlur = 14;
        ctx.fillStyle = '#ffffff';
        rr(ctx, bx, by, btnW, btnH, 17);
        ctx.fill();
        ctx.restore();

        ctx.strokeStyle = '#10b981';
        ctx.lineWidth = 1.6;
        rr(ctx, bx, by, btnW, btnH, 17);
        ctx.stroke();

        ctx.fillStyle = '#047857';
        ctx.font = '700 12px "Microsoft YaHei UI", sans-serif';
        ctx.textAlign = 'center';
        ctx.fillText('加载更多', bx + btnW / 2, by + btnH / 2 + 1);
        ctx.textAlign = 'left';
      }
    }
  }

  private drawPlayerScene(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    w: number,
    h: number,
    isMobile: boolean,
    t: number,
    mode: string
  ): void {
    const leftW = isMobile ? w : w * 0.66;
    const vidH = Math.min(h * 0.48, leftW * (9 / 16));

    // SwapChainPanel Video Surface
    const vg = ctx.createLinearGradient(x, y, x + leftW, y + vidH);
    vg.addColorStop(0, '#090d1a');
    vg.addColorStop(1, '#1e1b4b');
    ctx.fillStyle = vg;
    rr(ctx, x, y, leftW, vidH, 10);
    ctx.fill();

    // Animated Danmaku Overlay inside SwapChainPanel
    ctx.save();
    rr(ctx, x, y, leftW, vidH, 10);
    ctx.clip();
    DANMAKU_TRACKS.forEach((dm) => {
      const frac = ((t * dm.speed + dm.offset) % 1.4) - 0.2;
      const dx = x + leftW - frac * (leftW + 200);
      ctx.fillStyle = dm.color;
      ctx.font = '700 11px "Microsoft YaHei UI", sans-serif';
      ctx.fillText(dm.text, dx, y + vidH * dm.y);
    });

    // Bottom Play/Progress Strip
    ctx.fillStyle = 'rgba(2, 6, 23, 0.72)';
    ctx.fillRect(x, y + vidH - 28, leftW, 28);
    ctx.fillStyle = '#f8fafc';
    ctx.font = '700 10px monospace';
    ctx.fillText('▶  04:12 / 12:48', x + 10, y + vidH - 13);
    ctx.fillStyle = '#fb7299';
    rr(ctx, x + 106, y + vidH - 16, Math.max(20, (leftW - 122) * 0.36), 4, 2);
    ctx.fill();
    ctx.restore();

    if (isMobile) {
      // Tab strip: 简介 | 评论 2,847
      const tabY = y + vidH + 10;
      const showComments = mode === 'player-comments';
      ctx.fillStyle = showComments ? '#64748b' : '#0f172a';
      ctx.font = '700 13px "Microsoft YaHei UI", sans-serif';
      ctx.fillText('简介', x + 12, tabY + 12);

      ctx.fillStyle = showComments ? '#fb7299' : '#64748b';
      ctx.fillText('评论 2,847', x + 78, tabY + 12);

      // Underline indicator
      ctx.fillStyle = '#fb7299';
      rr(ctx, showComments ? x + 78 : x + 12, tabY + 24, showComments ? 64 : 26, 3, 2);
      ctx.fill();

      const bodyY = tabY + 36;
      if (!showComments) {
        ctx.fillStyle = '#0f172a';
        ctx.font = '700 12px "Microsoft YaHei UI", sans-serif';
        ctx.fillText('用 Rust + WinUI 3 重写竖屏桌面客户端', x + 8, bodyY + 12);
        if (mode === 'player-desc') {
          ctx.fillStyle = '#ffffff';
          rr(ctx, x + 4, bodyY + 26, w - 8, 76, 8);
          ctx.fill();
          ctx.fillStyle = '#475569';
          ctx.font = '11px "Microsoft YaHei UI", sans-serif';
          ctx.fillText('BV1QAQWinUI3 · 860 DIP 自动切换移动端交互', x + 14, bodyY + 48);
          ctx.fillText('零 Electron，实心 D3D11 SwapChain 视频直出。', x + 14, bodyY + 68);
        }
      } else {
        const comments = [
          'WinUI 3 + Rust 真香，内存控制得比 Electron 强太多！',
          '860px 变竖屏那一下太优雅了，摸鱼神形态',
          '连评论回复弹幕都接好了 OpenAPI，强！',
        ];
        comments.forEach((line, idx) => {
          const cy = bodyY + 6 + idx * 46;
          ctx.fillStyle = '#ffffff';
          rr(ctx, x + 4, cy, w - 8, 38, 8);
          ctx.fill();
          ctx.fillStyle = '#0f172a';
          ctx.font = '11px "Microsoft YaHei UI", sans-serif';
          const maxLen = Math.max(10, Math.floor((w - 32) / 12));
          const displayLine = line.length > maxLen ? `${line.slice(0, maxLen - 1)}…` : line;
          ctx.fillText(displayLine, x + 14, cy + 19);
        });
      }
    } else {
      // Desktop 2-Column Split: Comments below video left + Related Pane right!
      const cTop = y + vidH + 14;
      ctx.fillStyle = '#0f172a';
      ctx.font = '700 13px "Microsoft YaHei UI", sans-serif';
      ctx.fillText('全部评论 (2,847)', x + 4, cTop + 12);

      const desktopComments = [
        'QAQTam: 纯 Rust 重写 WinUI 3，内存稳稳压在 8G 机极低水位',
        'Overcoder: < 860 DIP 自动转手机竖屏，单双栏切换丝滑无感',
      ];
      desktopComments.forEach((line, idx) => {
        const cy = cTop + 26 + idx * 42;
        if (cy + 34 > y + h) return;
        ctx.fillStyle = '#ffffff';
        rr(ctx, x + 4, cy, leftW - 8, 34, 8);
        ctx.fill();
        ctx.fillStyle = '#334155';
        ctx.font = '11px "Microsoft YaHei UI", sans-serif';
        ctx.fillText(line, x + 14, cy + 17);
      });

      const rightX = x + leftW + 14;
      const rightW = w - leftW - 14;
      ctx.fillStyle = '#0f172a';
      ctx.font = '700 13px "Microsoft YaHei UI", sans-serif';
      ctx.fillText('接下来播放 · 相关推荐', rightX, y + 12);
      for (let i = 0; i < 4; i++) {
        const ry = y + 26 + i * 62;
        if (ry + 54 > y + h) break;
        const card = CARD_DATA[i % CARD_DATA.length];
        ctx.fillStyle = '#ffffff';
        rr(ctx, rightX, ry, rightW, 54, 8);
        ctx.fill();
        ctx.fillStyle = card.c1;
        rr(ctx, rightX + 6, ry + 6, 72, 42, 6);
        ctx.fill();
        ctx.fillStyle = '#0f172a';
        ctx.font = '600 10px "Microsoft YaHei UI", sans-serif';
        ctx.fillText(card.title.slice(0, 12) + '…', rightX + 86, ry + 20);
        ctx.fillStyle = '#64748b';
        ctx.fillText(card.author, rightX + 86, ry + 38);
      }
    }
  }
}
