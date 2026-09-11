// Trace one layout scenario frame by frame: which rows hold what after every
// step, and where the viewport top is. Used to diagnose layout bugs by reading
// instead of guessing.
//
// Usage: node scripts/layout-trace.js
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ESC = '\x1b';
const ROWS = 24;
const COLS = 70;
const ANSWER_ROWS = 4;
const INPUT = ROWS;
const VIEWPORT_TOP = ROWS - ANSWER_ROWS;

const tmp = path.join(os.tmpdir(), 'layout-trace.bin');

class FakeTerm {
  constructor() {
    this.s = `${ESC}[1;${ROWS}r${ESC}[1;1H`;
    this.viewportTop = VIEWPORT_TOP;
    this.streamActive = false;
    this.streamText = '';
    this.committed = 0;
  }
  snapshot(step) {
    fs.writeFileSync(tmp, this.s, 'utf8');
    const out = execFileSync('node', ['scripts/vtscreen.js', tmp, String(ROWS), String(COLS)], {
      encoding: 'utf8',
    });
    const lines = out.trimEnd().split('\n').slice(1).map((l) => l.slice(3).replace(/\s+$/, ''));
    const used = lines
      .map((l, i) => [i + 1, l])
      .filter(([, l]) => l !== '');
    console.log(
      `  ${step.padEnd(28)} viewportTop=${String(this.viewportTop).padStart(2)}  ` +
        used.map(([n, l]) => `${n}:${l}`).join('  ')
    );
  }
  redraw() {
    this.s += `${ESC}[${INPUT};1H${ESC}[2K${ESC}[1m> ${ESC}[0m`;
  }
  insertHistory(lines) {
    for (const text of lines) {
      const bottom = Math.max(this.viewportTop - 1, 1);
      this.s += `${ESC}[1;${bottom}r`;
      this.s += `${ESC}[${bottom};1H\r${ESC}[2K${text}\r\n`;
      this.s += `${ESC}[r`;
    }
    this.s += `${ESC}[?25l`;
    this.redraw();
  }
  line(text) {
    this.closeStream();
    this.insertHistory([text]);
  }
  endStream() {
    this.closeStream();
  }
  scrollHistoryDown(rows) {
    if (rows === 0) return;
    this.s += `${ESC}[1;${ROWS}r${ESC}[1;1H`;
    for (let i = 0; i < rows; i++) this.s += `${ESC}M`;
    this.s += `${ESC}[r`;
  }
  clearViewport() {
    for (let row = this.viewportTop; row < INPUT; row++) {
      this.s += `${ESC}[${row};1H${ESC}[2K`;
    }
  }
  closeStream() {
    if (!this.streamActive) return;
    this.streamActive = false;
    if (!this.streamText) return;
    const lines = this.streamText.split('\n');
    const top = VIEWPORT_TOP;
    const last = INPUT - 1;
    const capacity = last - top + 1;
    const shown = Math.min(lines.length, capacity);
    // Hand the lines still on screen to history first, so nothing is lost, then
    // blank the slice. Only after that is it safe to scroll: every row the scroll
    // moves is already empty, so no ordering subtlety can lose or duplicate a line.
    const left = lines.slice(lines.length - shown);
    this.insertHistory(left);
    for (let r = top; r <= last; r++) this.s += `${ESC}[${r};1H${ESC}[2K`;
    this.s += `${ESC}[${top};${last}r\x1b[${top};1H`;
    for (let i = 0; i < shown; i++) this.s += `\r\n`;
    this.s += `${ESC}[r`;
    this.committed = 0;
  }
  stream(text) {
    this.streamActive = true;
    this.streamText = text;
    const rows = text.split('\n');
    const height = rows.length;
    const top = VIEWPORT_TOP;
    const last = INPUT - 1;
    const capacity = last - top + 1;

    // The viewport is a fixed slice sitting on the input row and it never moves.
    // The answer is drawn bottom-anchored inside it; lines that no longer fit at
    // the top of the slice are handed to history in order, so the transcript keeps
    // the whole answer even though the slice only ever shows its tail.
    if (height > capacity) {
      const drop = height - capacity;
      if (drop > this.committed) {
        this.insertHistory(rows.slice(this.committed, drop));
        this.committed = drop;
      }
    }
    const first = Math.max(0, height - capacity);
    const visible = rows.slice(first);
    const start = top + (capacity - visible.length);
    let cursor = null;
    for (let n = 0; n < visible.length; n++) {
      const r = start + n;
      if (r > last) break;
      this.s += `${ESC}[${r};1H${ESC}[2K${visible[n]}`;
      cursor = [r, [...visible[n]].length];
    }
    if (cursor) this.s += `${ESC}[${cursor[0]};${Math.max(cursor[1], 1)}H`;
    this.s += `${ESC}[?25l`;
    this.redraw();
  }
}

const t = new FakeTerm();
t.line('> 讲故事');
t.snapshot('after question');
let acc = '';
for (let i = 1; i <= 8; i++) {
  acc += (i > 1 ? '\n' : '') + `第 ${i} 行回答`;
  t.stream(acc);
  t.snapshot(`stream ${i} lines`);
}
t.endStream();
t.snapshot('after end_stream');
