export const TOTAL_DURATION = 20.0;

export interface BeatCue {
  start: number;
  end: number;
  hudAct: string;
  caption: string;
  callout: string;
  headerKicker: string;
  headerTitle: string;
  headerSub: string;
}

export const BEAT_CUES: BeatCue[] = [
  {
    start: 0.0,
    end: 2.5,
    hudAct: '01 // COVER SLAM',
    caption: 'Overcoder！胡闹编程 —— 竖屏 WinUI 客户端',
    callout: '',
    headerKicker: 'OVERCODER! // WINUI 3 IN RUST',
    headerTitle: '竖屏 WinUI 客户端',
    headerSub: '纯 Rust + WinUI 3 · Mica 质感与双态布局',
  },
  {
    start: 2.5,
    end: 5.8,
    hudAct: '02 // 860 DIP MORPH',
    caption: '宽度越过 860 DIP，桌面 4 列卡片瞬间坍缩为竖屏 2 列',
    callout: 'Breakpoint 860 → BottomNav + 2 列栅格',
    headerKicker: 'BREAKPOINT // < 860 DIP AUTO-COLLAPSE',
    headerTitle: '1280 → 390 DIP 丝滑自适应',
    headerSub: '左侧栏自动收起，底栏 + 双列竖屏卡片接管',
  },
  {
    start: 5.8,
    end: 8.6,
    hudAct: '03 // POPULAR + MORE',
    caption: '修复竖屏热门「加载更多」裁切重叠 · 2×3 悬浮更多菜单',
    callout: '已修复 · 加载更多按钮居中自适应不遮挡',
    headerKicker: 'MOBILE ROUTE // 热门与 2×3 菜单',
    headerTitle: '热门页修复 · 加载更多清晰不重叠',
    headerSub: '390 DIP 竖屏底栏直达，点击更多弹出 2×3 快捷网格',
  },
  {
    start: 8.6,
    end: 11.4,
    hudAct: '04 // FLOAT SEARCH',
    caption: '640 DIP 窄桌面标题栏弹出极简浮窗搜索：Rust WinUI',
    callout: 'Medium 600~859 · 标题栏悬浮搜索框',
    headerKicker: 'MEDIUM TIER // FLOATING SEARCH OVERLAY',
    headerTitle: '窄桌面保留通知 · 悬浮浮窗搜全站',
    headerSub: '600 ~ 859 DIP 专属交互，释放宝贵纵向空间',
  },
  {
    start: 11.4,
    end: 14.5,
    hudAct: '05 // STICKY PLAYER',
    caption: '竖屏播放页：16:9 吸顶播放区 + 简介展开 / 评论区切换',
    callout: 'SwapChainPanel 吸顶 · 简介 | 评论 双标签',
    headerKicker: 'MOBILE PLAYER // 16:9 STICKY TAB VIEW',
    headerTitle: '16:9 视频吸顶 · 简介评论秒切',
    headerSub: '手机端收起右侧推荐流，聚焦弹幕播放与互动',
  },
  {
    start: 14.5,
    end: 20.0,
    hudAct: '06 // DESKTOP FINALE',
    caption: '拉宽还原 1280×800 双栏桌面播放器 · 纯正 Rust 原生速度',
    callout: '1280×800 · 双栏播放页无感恢复',
    headerKicker: 'EXPAND BACK // 390 → 1280 DESKTOP SPLIT',
    headerTitle: '拉宽秒回双栏桌面端',
    headerSub: '4c8T · 8G 内存 · 零 Electron 原生客户端',
  },
];

export interface ViewportKeyframe {
  t: number;
  w: number;
  h: number;
  mode: 'home' | 'popular' | 'more' | 'search' | 'player-intro' | 'player-desc' | 'player-comments';
}

export const VIEWPORT_KEYFRAMES: ViewportKeyframe[] = [
  { t: 0.0, w: 1280, h: 800, mode: 'home' },
  { t: 2.5, w: 1280, h: 800, mode: 'home' },
  { t: 4.3, w: 640, h: 760, mode: 'home' },
  { t: 5.5, w: 390, h: 760, mode: 'home' },
  { t: 5.8, w: 390, h: 760, mode: 'popular' },
  { t: 7.4, w: 390, h: 760, mode: 'more' },
  { t: 8.6, w: 640, h: 760, mode: 'search' },
  { t: 11.2, w: 640, h: 760, mode: 'search' },
  { t: 11.7, w: 390, h: 760, mode: 'player-intro' },
  { t: 12.8, w: 390, h: 760, mode: 'player-desc' },
  { t: 13.7, w: 390, h: 760, mode: 'player-comments' },
  { t: 14.5, w: 390, h: 760, mode: 'player-comments' },
  { t: 15.8, w: 1280, h: 800, mode: 'player-comments' },
  { t: 20.0, w: 1280, h: 800, mode: 'player-comments' },
];

export function cueAt(t: number): BeatCue {
  for (const cue of BEAT_CUES) {
    if (t >= cue.start && t < cue.end) return cue;
  }
  return BEAT_CUES[BEAT_CUES.length - 1];
}

function easeInOutCubic(x: number): number {
  const c = Math.max(0, Math.min(1, x));
  return c < 0.5 ? 4 * c * c * c : 1 - Math.pow(-2 * c + 2, 3) / 2;
}

export function sampleViewportAt(t: number): {
  w: number;
  h: number;
  cols: number;
  chrome: 'DESKTOP' | 'MEDIUM' | 'MOBILE';
  mode: ViewportKeyframe['mode'];
} {
  let w = 1280;
  let h = 800;
  let mode: ViewportKeyframe['mode'] = 'home';

  for (let i = 0; i < VIEWPORT_KEYFRAMES.length - 1; i++) {
    const a = VIEWPORT_KEYFRAMES[i];
    const b = VIEWPORT_KEYFRAMES[i + 1];
    if (t >= a.t && t <= b.t) {
      const span = Math.max(0.001, b.t - a.t);
      const k = easeInOutCubic((t - a.t) / span);
      w = Math.round(a.w + (b.w - a.w) * k);
      h = Math.round(a.h + (b.h - a.h) * k);
      mode = a.mode;
      break;
    }
    if (t > b.t) {
      w = b.w;
      h = b.h;
      mode = b.mode;
    }
  }

  const chrome: 'DESKTOP' | 'MEDIUM' | 'MOBILE' = w >= 860 ? 'DESKTOP' : w >= 600 ? 'MEDIUM' : 'MOBILE';
  const cols = w >= 1200 ? 4 : w >= 860 ? 3 : 2;
  return { w, h, cols, chrome, mode };
}
