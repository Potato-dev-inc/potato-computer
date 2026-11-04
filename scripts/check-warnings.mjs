// Fails when the Rust app crate or the frontend prints any warning. Run: npm run check:warnings
// Plain `cargo check` output is counted, so this never forces a rebuild the way -D warnings would.
import { spawnSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const app = join(root, 'packages', 'app');
const run = (cmd, cwd) => {
  const r = spawnSync(cmd, { cwd, shell: true, encoding: 'utf8', maxBuffer: 1 << 28 });
  return { ok: r.status === 0, text: `${r.stdout}\n${r.stderr}` };
};
let bad = 0;
const report = (name, ok, lines) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
  if (!ok) { bad++; for (const l of lines.slice(0, 40)) console.log(`     ${l}`); }
};

const cargo = run('cargo check --lib --no-default-features --message-format=short', join(app, 'src-tauri'));
const rust = cargo.text.split('\n').filter((l) => /: (warning|error)/.test(l));
report(`rust: ${rust.length} warnings`, cargo.ok && rust.length === 0, rust);

const tsc = run('npx tsc --noEmit -p tsconfig.json', app);
report('typescript', tsc.ok, tsc.text.split('\n').filter(Boolean));

const out = mkdtempSync(join(tmpdir(), 'pm-vite-'));
const vite = run(`npx vite build --outDir "${out}" --emptyOutDir`, app);
const web = vite.text.split('\n').filter((l) => /\(!\)|\bwarning\b/i.test(l));
report(`vite: ${web.length} warnings`, vite.ok && web.length === 0, web);

process.exit(bad ? 1 : 0);
