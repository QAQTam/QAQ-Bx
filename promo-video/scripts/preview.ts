import { join } from 'node:path';

const root = join(import.meta.dir, '..');
const distDir = join(root, 'dist');

const buildProc = Bun.spawnSync(['bun', 'run', 'scripts/build.ts'], {
  cwd: root,
  stdout: 'inherit',
  stderr: 'inherit',
});
if (buildProc.exitCode !== 0) process.exit(buildProc.exitCode);

function parsePort(): number | null {
  for (const arg of process.argv.slice(2)) {
    if (arg.startsWith('--port=')) {
      const p = Number(arg.slice('--port='.length));
      if (Number.isInteger(p) && p >= 0 && p <= 65535) return p;
    }
  }
  return null;
}

const server = Bun.serve({
  port: parsePort() ?? 4185,
  async fetch(req) {
    const url = new URL(req.url);
    const rel = url.pathname === '/' ? 'index.html' : decodeURIComponent(url.pathname).replace(/^\/+/, '');
    const file = Bun.file(join(distDir, rel));
    if (await file.exists()) {
      return new Response(file, {
        headers: { 'Content-Type': file.type, 'Cache-Control': 'no-store' },
      });
    }
    return new Response('Not Found', { status: 404 });
  },
});

console.log(`Promo studio preview: http://127.0.0.1:${server.port}/`);
console.log('Shortcuts: Space=Play/Pause | Left/Right=+-0.5s');
