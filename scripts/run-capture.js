// Run flint with given arguments, capture stdout/stderr as raw bytes.
//
// Usage: node scripts/run-capture.js <out-file> <exe> [args...]
const { spawn } = require('child_process');
const fs = require('fs');

const outFile = process.argv[2];
const exe = process.argv[3];
const args = process.argv.slice(4);

// stdin is a pipe that is closed immediately: the one-shot path must not need a
// terminal, and leaving stdin open would look like an interactive session.
const child = spawn(exe, args, { stdio: ['pipe', 'pipe', 'pipe'] });
child.stdin.end();

const chunks = [];
child.stdout.on('data', (c) => chunks.push(c));
child.stderr.on('data', (c) => chunks.push(c));

child.on('exit', (code) => {
  const buf = Buffer.concat(chunks);
  fs.writeFileSync(outFile, buf);
  let esc = 0;
  for (const b of buf) if (b === 27) esc++;
  const text = buf.toString('utf8');
  console.log(`exit=${code} bytes=${buf.length} esc=${esc}`);
  console.log('--- stdout+stderr ---');
  console.log(text);
});
