// Drive flint's interactive REPL through a Windows pseudo-console.
//
// Raw mode and key events need a real console; a pipe will not do, because
// crossterm reads the console rather than stdin. Node's `conpty: true` option
// allocates a pseudo-console, which is what makes the reserved input row and the
// DECSTBM scroll region testable without a human at the keyboard.
//
// Usage: node scripts/interactive-check.js <out-file> <exe> <script-file> [keyDelayMs]
//   Each non-empty line of <script-file> is typed, then Enter.
const { spawn } = require('child_process');
const fs = require('fs');

const outFile = process.argv[2];
const exe = process.argv[3];
const scriptFile = process.argv[4];
const keyDelay = parseInt(process.argv[5] || '800', 10);

const lines = fs
  .readFileSync(scriptFile, 'utf8')
  .split(/\r?\n/)
  .filter((l) => l.length > 0);

const child = spawn(exe, [], {
  stdio: ['pipe', 'pipe', 'pipe'],
  conpty: true,
});

let out = '';
child.stdout.on('data', (c) => (out += c.toString('utf8')));
child.stderr.on('data', (c) => (out += c.toString('utf8')));

function typeLine(i) {
  if (i >= lines.length) {
    setTimeout(() => child.kill(), 1500);
    return;
  }
  child.stdin.write(lines[i] + '\r');
  setTimeout(() => typeLine(i + 1), keyDelay);
}

setTimeout(() => typeLine(0), 1500);

child.on('exit', () => {
  fs.writeFileSync(outFile, Buffer.from(out, 'utf8'));
  let esc = 0;
  for (const b of Buffer.from(out, 'utf8')) if (b === 27) esc++;
  console.log(`bytes=${out.length} esc=${esc}`);
});
