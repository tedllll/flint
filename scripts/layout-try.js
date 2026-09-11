// Compare candidate output sequences by replaying them.
//
// Usage: node scripts/layout-try.js
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ESC = '\x1b';
const ROWS = 24;
const COLS = 60;
const BOTTOM = ROWS - 1; // 23: last row of the scroll region
const INPUT = ROWS; // 24: the reserved row

const tmp = path.join(os.tmpdir(), 'layout-try.bin');

function render(name, build) {
  const bytes = build();
  fs.writeFileSync(tmp, bytes, 'utf8');
  const out = execFileSync('node', ['scripts/vtscreen.js', tmp, String(ROWS), String(COLS)], {
    encoding: 'utf8',
  });
  console.log(`\n===== ${name} =====`);
  console.log(out.trimEnd());
}

// A: redraw leaves the cursor on the input row, and output is written there.
//    This is what the current code does.
render('A: 输出直接写在 redraw 停留处', () => {
  let s = `${ESC}[1;${BOTTOM}r${ESC}[1;1H`;
  for (let i = 1; i <= 5; i++) {
    s += `line ${i}`;
    s += '\r\n';
    s += `${ESC}[?25l`;
    s += `${ESC}[${INPUT};1H${ESC}[2K${ESC}[1m> ${ESC}[0m`;
  }
  return s;
});

// B: after redrawing the input row, put the cursor back on the last row of the
//    scroll region. A write there scrolls the region and the cursor stays there,
//    so no state has to be tracked.
render('B: redraw 后把光标放回滚动区末行', () => {
  let s = `${ESC}[1;${BOTTOM}r${ESC}[1;1H`;
  for (let i = 1; i <= 5; i++) {
    s += `line ${i}`;
    s += '\r\n';
    s += `${ESC}[?25l`;
    s += `${ESC}[${INPUT};1H${ESC}[2K${ESC}[1m> ${ESC}[0m`;
    s += `${ESC}[${BOTTOM};1H`;
  }
  return s;
});

// C: same as B, but the cursor is restored before the newline instead of after
//    the redraw, so the line is written where the cursor already sits.
render('C: 每行先回末行，再写，再换行，再 redraw', () => {
  let s = `${ESC}[1;${BOTTOM}r${ESC}[1;1H`;
  for (let i = 1; i <= 5; i++) {
    s += `${ESC}[${BOTTOM};1H`;
    s += `line ${i}`;
    s += '\r\n';
    s += `${ESC}[?25l`;
    s += `${ESC}[${INPUT};1H${ESC}[2K${ESC}[1m> ${ESC}[0m`;
  }
  return s;
});
