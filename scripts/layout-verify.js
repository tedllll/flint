// Verify the chosen layout under more lines than the region holds, and with a
// multi-line write (which is what `blank()` and `list` output produce).
//
// Usage: node scripts/layout-verify.js
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ESC = '\x1b';
const ROWS = 24;
const COLS = 60;
const BOTTOM = ROWS - 1;
const INPUT = ROWS;

const tmp = path.join(os.tmpdir(), 'layout-verify.bin');

function render(name, build) {
  fs.writeFileSync(tmp, build(), 'utf8');
  const out = execFileSync('node', ['scripts/vtscreen.js', tmp, String(ROWS), String(COLS)], {
    encoding: 'utf8',
  });
  console.log(`\n===== ${name} =====`);
  console.log(out.trimEnd());
}

// The chosen arrangement: cursor returns to the region's last row for each write.
function session(writes) {
  let s = `${ESC}[1;${BOTTOM}r${ESC}[1;1H`;
  for (const w of writes) {
    s += `${ESC}[${BOTTOM};1H`;
    for (const line of w.split('\n')) {
      s += line + '\r\n';
    }
    s += `${ESC}[?25l`;
    s += `${ESC}[${INPUT};1H${ESC}[2K${ESC}[1m> ${ESC}[0m`;
  }
  return s;
}

// 40 lines: the region holds 22, so this must scroll and lose the oldest.
render('40 行输出（滚动区只有 22 行可见）', () =>
  session(Array.from({ length: 40 }, (_, i) => `line ${i + 1}`))
);

// A multi-line write, which is what `list` and `blank()` produce.
render('一个多行写入（含空行）', () => session(['a\nb\n\nc\nd']));

// The first line the user actually sees, right after setup.
render('启动后的第一行', () => session(['flint v0.1.0  deepseek/deepseek-flash']));
