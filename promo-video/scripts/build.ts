import { existsSync } from 'node:fs';
import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const root = join(import.meta.dir, '..');
const distDir = join(root, 'dist');
const assetsDir = join(root, 'assets');

function findFfmpeg(): string {
  const inPath = Bun.which('ffmpeg');
  if (inPath) return inPath;
  const localApp = process.env.LOCALAPPDATA ?? '';
  const fallback = join(localApp, 'Microsoft', 'WinGet', 'Links', 'ffmpeg.exe');
  if (existsSync(fallback)) return fallback;
  return 'ffmpeg';
}

async function prepareDemoFrames(): Promise<void> {
  const rawMp4 = join(assetsDir, 'raw-demo.mp4');
  const framesDir = join(distDir, 'demo-frames');
  if (!existsSync(rawMp4)) {
    if (existsSync(framesDir)) {
      await rm(framesDir, { recursive: true, force: true });
    }
    return;
  }

  await rm(framesDir, { recursive: true, force: true });
  await mkdir(framesDir, { recursive: true });
  const fps = 30;
  const ffmpeg = findFfmpeg();
  const proc = Bun.spawnSync(
    [ffmpeg, '-y', '-i', rawMp4, '-vf', `fps=${fps}`, '-q:v', '3', join(framesDir, 'f_%04d.jpg')],
    { stdout: 'ignore', stderr: 'pipe' }
  );
  if (proc.exitCode !== 0) {
    console.warn('[build] ffmpeg extract of assets/raw-demo.mp4 failed; falling back to Canvas UI:');
    console.warn(proc.stderr.toString());
    return;
  }

  const files = (await readdir(framesDir)).filter((f) => f.endsWith('.jpg'));
  const manifest = {
    count: files.length,
    fps,
    duration: files.length / fps,
  };
  await writeFile(join(framesDir, 'manifest.json'), JSON.stringify(manifest), 'utf8');
  console.log(`[build] Extracted ${files.length} desktop WinUI frames from assets/raw-demo.mp4`);
}

async function build(): Promise<void> {
  await mkdir(distDir, { recursive: true });

  const result = await Bun.build({
    entrypoints: [join(root, 'src', 'main.ts')],
    outdir: distDir,
    minify: true,
    target: 'browser',
    format: 'esm',
    naming: 'main.js',
  });

  if (!result.success) {
    for (const log of result.logs) console.error(log);
    process.exit(1);
  }

  if (existsSync(assetsDir)) {
    await cp(assetsDir, join(distDir, 'assets'), { recursive: true, force: true });
  }

  const rawHtml = await readFile(join(root, 'index.html'), 'utf8');
  const rawCss = await readFile(join(root, 'src', 'styles.css'), 'utf8');
  await writeFile(join(distDir, 'styles.css'), rawCss, 'utf8');

  const distHtml = rawHtml
    .replace('./src/styles.css', './styles.css')
    .replace('./src/main.ts', './main.js');
  await writeFile(join(distDir, 'index.html'), distHtml, 'utf8');

  await prepareDemoFrames();
  console.log('[build] Built promo-video/dist cleanly.');
}

await build();
