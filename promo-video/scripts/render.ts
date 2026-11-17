import { existsSync } from 'node:fs';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const root = join(import.meta.dir, '..');
const distDir = join(root, 'dist');
const outDir = join(root, 'out');

type Ratio = '9:16' | '16:9';

interface Options {
  ratio: Ratio;
  fps: number;
  smoke: boolean;
}

function parseArgs(): Options {
  let ratio: Ratio = '9:16';
  let fps = 30;
  let smoke = false;
  for (const arg of process.argv.slice(2)) {
    if (arg === '--smoke') smoke = true;
    else if (arg === '--ratio=16:9') ratio = '16:9';
    else if (arg === '--ratio=9:16') ratio = '9:16';
    else if (arg.startsWith('--fps=')) {
      const n = Number(arg.slice('--fps='.length));
      if (Number.isFinite(n) && n >= 15 && n <= 60) fps = Math.round(n);
    }
  }
  return { ratio, fps, smoke };
}

function findBrowser(): string {
  const candidates = [
    process.env.CHROME_EXE,
    'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
    'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe',
    Bun.which('chrome') ?? undefined,
    Bun.which('msedge') ?? undefined,
  ].filter((x): x is string => Boolean(x));

  for (const path of candidates) {
    if (existsSync(path)) return path;
  }
  throw new Error('No Chromium/Chrome/Edge executable found for headless CDP render.');
}

function findFfmpeg(): string {
  const inPath = Bun.which('ffmpeg');
  if (inPath) return inPath;
  const fallback = join(process.env.LOCALAPPDATA ?? '', 'Microsoft', 'WinGet', 'Links', 'ffmpeg.exe');
  if (existsSync(fallback)) return fallback;
  throw new Error('ffmpeg executable not found.');
}

class CdpClient {
  private ws: WebSocket;
  private nextId = 1;
  private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();

  private constructor(ws: WebSocket) {
    this.ws = ws;
    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(String(ev.data)) as { id?: number; result?: unknown; error?: { message: string } };
      if (typeof msg.id === 'number') {
        const entry = this.pending.get(msg.id);
        if (entry) {
          this.pending.delete(msg.id);
          if (msg.error) entry.reject(new Error(msg.error.message));
          else entry.resolve(msg.result);
        }
      }
    });
  }

  static async connect(wsUrl: string): Promise<CdpClient> {
    const ws = new WebSocket(wsUrl);
    await new Promise<void>((resolve, reject) => {
      ws.addEventListener('open', () => resolve(), { once: true });
      ws.addEventListener('error', (e) => reject(new Error(`CDP WebSocket error: ${String(e)}`)), { once: true });
    });
    return new CdpClient(ws);
  }

  send<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async evalJs<T = unknown>(expression: string): Promise<T> {
    const res = await this.send<{ result: { value: T }; exceptionDetails?: { text: string } }>('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (res.exceptionDetails) {
      throw new Error(`Page eval error: ${res.exceptionDetails.text}`);
    }
    return res.result?.value;
  }

  close(): void {
    this.ws.close();
  }
}

async function waitForCdpTarget(port: number): Promise<string> {
  const deadline = Date.now() + 12000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/json/list`);
      if (res.ok) {
        const targets = (await res.json()) as Array<{ type: string; webSocketDebuggerUrl?: string }>;
        const page = targets.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
        if (page?.webSocketDebuggerUrl) return page.webSocketDebuggerUrl;
      }
    } catch {
      // Retry until Chromium opens debugging port
    }
    await Bun.sleep(100);
  }
  throw new Error('Timed out waiting for Chromium CDP endpoint.');
}

async function run(): Promise<void> {
  const opts = parseArgs();
  const buildProc = Bun.spawnSync(['bun', 'run', 'scripts/build.ts'], {
    cwd: root,
    stdout: 'inherit',
    stderr: 'inherit',
  });
  if (buildProc.exitCode !== 0) process.exit(buildProc.exitCode);

  await mkdir(outDir, { recursive: true });
  const wavPath = join(outDir, 'tmp-audio.wav');

  const server = Bun.serve({
    port: 0,
    async fetch(req) {
      const url = new URL(req.url);
      if (req.method === 'POST' && url.pathname === '/upload-audio') {
        const bytes = await req.arrayBuffer();
        await writeFile(wavPath, Buffer.from(bytes));
        return new Response('OK');
      }
      const rel = url.pathname === '/' ? 'index.html' : decodeURIComponent(url.pathname).replace(/^\/+/, '');
      const file = Bun.file(join(distDir, rel));
      if (await file.exists()) {
        return new Response(file, { headers: { 'Content-Type': file.type, 'Cache-Control': 'no-store' } });
      }
      return new Response('Not Found', { status: 404 });
    },
  });

  const width = opts.ratio === '9:16' ? 1080 : 1920;
  const height = opts.ratio === '9:16' ? 1920 : 1080;
  const ratioTag = opts.ratio === '9:16' ? '9x16' : '16x9';
  const cdpPort = 19220 + Math.floor(Math.random() * 500);
  const profileDir = join(tmpdir(), `overcoder-promo-cdp-${cdpPort}`);
  await mkdir(profileDir, { recursive: true });

  const chromeExe = findBrowser();
  const browserProc = Bun.spawn(
    [
      chromeExe,
      '--headless=new',
      '--disable-gpu',
      '--no-sandbox',
      '--mute-audio',
      `--remote-debugging-port=${cdpPort}`,
      `--window-size=${width},${height}`,
      `--user-data-dir=${profileDir}`,
      'about:blank',
    ],
    { stdout: 'ignore', stderr: 'ignore' }
  );

  try {
    const wsUrl = await waitForCdpTarget(cdpPort);
    const cdp = await CdpClient.connect(wsUrl);
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });

    await cdp.send('Page.navigate', {
      url: `http://127.0.0.1:${server.port}/?render=1&ratio=${encodeURIComponent(opts.ratio)}`,
    });
    await Bun.sleep(650);

    // Wait for font/image decoding + window.__seek registration
    await cdp.evalJs(
      `new Promise((res) => { const check = () => (window.__seek && document.readyState === 'complete' ? res(true) : setTimeout(check, 50)); check(); })`
    );
    await cdp.evalJs(`Promise.all(Array.from(document.images).map(i => i.complete ? null : new Promise(r => { i.onload = r; i.onerror = r; })))`);

    // 1. Export cover poster PNG cleanly
    await cdp.evalJs(`window.__setCoverMode(true)`);
    await Bun.sleep(80);
    const coverShot = await cdp.send<{ data: string }>('Page.captureScreenshot', { format: 'png' });
    const coverPath = join(outDir, `cover-${ratioTag}.png`);
    await writeFile(coverPath, Buffer.from(coverShot.data, 'base64'));
    console.log(`[render] Saved cover poster -> ${coverPath}`);
    await cdp.evalJs(`window.__setCoverMode(false)`);

    if (opts.smoke) {
      const smokeDir = join(outDir, 'smoke');
      await mkdir(smokeDir, { recursive: true });
      const keyframes = [0.6, 3.6, 6.4, 7.6, 9.6, 12.2, 13.9, 15.6, 18.2];
      for (const kf of keyframes) {
        await cdp.evalJs(`window.__seek(${kf})`);
        await Bun.sleep(40);
        const shot = await cdp.send<{ data: string }>('Page.captureScreenshot', { format: 'png' });
        const name = `kf_${kf.toFixed(1).replace('.', '_')}s_${ratioTag}.png`;
        await writeFile(join(smokeDir, name), Buffer.from(shot.data, 'base64'));
      }
      console.log(`[render] Captured ${keyframes.length} smoke keyframes -> ${smokeDir}`);
      cdp.close();
      return;
    }

    // 2. Render offline 20s commercial audio WAV
    const wavBytes = await cdp.evalJs<number>(
      `window.__renderOfflineAudioMix("http://127.0.0.1:${server.port}/upload-audio")`
    );
    console.log(`[render] Synthesized ${(wavBytes / 1024 / 1024).toFixed(2)} MB WAV track`);

    // 3. Pipe 20s frames into ffmpeg
    const duration = await cdp.evalJs<number>(`window.__OVEROCODER_DURATION`);
    const totalFrames = Math.ceil(duration * opts.fps);
    const outMp4 = join(outDir, `overcoder-winui-${ratioTag}.mp4`);
    const ffmpeg = findFfmpeg();

    const ff = Bun.spawn(
      [
        ffmpeg,
        '-y',
        '-f',
        'image2pipe',
        '-vcodec',
        'mjpeg',
        '-framerate',
        String(opts.fps),
        '-i',
        'pipe:0',
        '-i',
        wavPath,
        '-c:v',
        'libx264',
        '-preset',
        'medium',
        '-crf',
        '18',
        '-pix_fmt',
        'yuv420p',
        '-c:a',
        'aac',
        '-b:a',
        '192k',
        '-shortest',
        '-movflags',
        '+faststart',
        outMp4,
      ],
      { stdin: 'pipe', stdout: 'inherit', stderr: 'pipe' }
    );

    for (let f = 0; f < totalFrames; f++) {
      const t = Math.min(duration, f / opts.fps);
      await cdp.evalJs(`window.__seek(${t.toFixed(5)})`);
      const shot = await cdp.send<{ data: string }>('Page.captureScreenshot', {
        format: 'jpeg',
        quality: 92,
      });
      ff.stdin.write(Buffer.from(shot.data, 'base64'));
      if (f % (opts.fps * 2) === 0) {
        console.log(`[render] Encoding ${ratioTag} @ ${opts.fps}fps: ${f}/${totalFrames} (${((f / totalFrames) * 100).toFixed(0)}%)`);
      }
    }
    ff.stdin.end();
    const exitCode = await ff.exited;
    if (exitCode !== 0) {
      const errText = await new Response(ff.stderr).text();
      throw new Error(`ffmpeg failed (${exitCode}):\n${errText}`);
    }
    console.log(`[render] Complete MP4 -> ${outMp4}`);
    cdp.close();
  } finally {
    browserProc.kill();
    server.stop(true);
    await rm(profileDir, { recursive: true, force: true }).catch(() => {});
  }
}

await run();
