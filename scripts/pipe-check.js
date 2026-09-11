// Drive flint with a pipe on stdin while capturing its stdout as raw bytes.
//
// The point is to check for escape codes on the non-terminal path, so the capture
// must not re-encode anything: Node writes the child's bytes straight to a file.
//
// Usage: node scripts/pipe-check.js <flint.exe> <input-file> <out-file>
const { spawn } = require('child_process');
const fs = require('fs');

const exe = process.argv[2];
const inputFile = process.argv[3];
const outFile = process.argv[4];

const child = spawn(exe, ['--no-color'], { stdio: ['pipe', 'pipe', 'pipe'] });

const chunks = [];
child.stdout.on('data', (c) => chunks.push(c));
child.stderr.on('data', (c) => chunks.push(c));

setTimeout(() => {
  child.stdin.write(fs.readFileSync(inputFile));
  child.stdin.end();
}, 300);

child.on('exit', (code) => {
  const buf = Buffer.concat(chunks);
  fs.writeFileSync(outFile, buf);
  let esc = 0;
  for (const b of buf) if (b === 27) esc++;
  console.log(`exit=${code} bytes=${buf.length} esc=${esc}`);
});
