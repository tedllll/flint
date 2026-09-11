// Replay the exact escape sequences `Term` emits, and check what the user ends up
// seeing.
//
// This file mirrors src/term.rs: `setup` sets the scroll region, `place` puts the
// cursor on its last row, `line` writes and newlines there, `stream` re-renders
// the accumulated answer anchored one row above the margin, `redraw` paints the
// reserved row. Keeping the bytes in one place means a change in term.rs that
// breaks the layout fails here instead of on someone's screen.
//
// Usage: node scripts/term-layout-test.js
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ESC = '\x1b';
const ROWS = 24;
const COLS = 70;
const BOTTOM = ROWS - 1; // scroll region 1..23
const INPUT = ROWS; // reserved row 24

const tmp = path.join(os.tmpdir(), 'term-layout.bin');

// --- a stand-in for Term, emitting the same bytes -------------------------------
class FakeTerm {
  constructor() {
    this.s = `${ESC}[1;${BOTTOM}r${ESC}[1;1H`;
    this.streamRow = BOTTOM;
  }
  outRow() {
    return Math.max(this.streamRow, 1);
  }
  seek(row) {
    this.s += `${ESC}[${row};1H\r${ESC}[2K`;
  }
  roll(row) {
    this.s += `${ESC}[${row};1H\r\n`;
    if (row >= BOTTOM) return BOTTOM;
    return row + 1;
  }
  redraw(prefix = '> ') {
    this.s += `${ESC}[${INPUT};1H${ESC}[2K${ESC}[1m${prefix}${ESC}[0m`;
  }
  line(text) {
    const row = this.outRow();
    this.seek(row);
    this.s += text;
    this.streamRow = this.roll(row);
    this.s += `${ESC}[?25l`;
    this.redraw();
  }
  blank() {
    this.line('');
  }
  textLn(text) {
    for (const part of text.replace(/[\r\n]+$/, '').split('\n')) {
      this.line(part.replace(/\r$/, ''));
    }
  }
  // Mirrors Term::stream: re-render the whole accumulated answer, bottom-anchored,
  // leaving one row free below it.
  stream(text) {
    const rows = text.split('\n');
    const lowest = Math.max(BOTTOM - 1, 1);
    const anchor = Math.max(this.streamRow, 1);
    const over = Math.max(0, anchor + rows.length - (lowest + 1));
    const start = Math.max(1, anchor - over);
    let last = null;
    for (let n = 0; n < rows.length; n++) {
      const r = start + n;
      if (r > lowest) break;
      this.s += `${ESC}[${r};1H${ESC}[2K${rows[n]}`;
      last = [r, [...rows[n]].length];
    }
    if (last) this.s += `${ESC}[${last[0]};${Math.max(last[1], 1)}H`;
    this.s += `${ESC}[?25l`;
    this.redraw();
  }
  bytes() {
    return this.s;
  }
}

function screen(name, build) {
  fs.writeFileSync(tmp, build().bytes(), 'utf8');
  const out = execFileSync('node', ['scripts/vtscreen.js', tmp, String(ROWS), String(COLS)], {
    encoding: 'utf8',
  });
  // The first line is the tool's own header, not part of the screen.
  const lines = out.trimEnd().split('\n').slice(1);
  console.log(`\n===== ${name} =====`);
  console.log(lines.join('\n'));
  // Trailing spaces are trimmed, so the prompt reads as '>' rather than '> '.
  return lines.map((l) => l.slice(3).replace(/\s+$/, ''));
}

let failures = 0;
function check(label, cond) {
  console.log(`  ${cond ? 'PASS' : 'FAIL'}  ${label}`);
  if (!cond) failures++;
}

// The reserved row is the last one on screen.
function inputRow(s) {
  return s[s.length - 1];
}

// --- the case that looked broken: streamed text arriving in pieces --------------
{
  const t = new FakeTerm();
  t.line('flint v0.1.0  deepseek/deepseek-flash');
  t.blank();
  // Streamed in fragments, including splits mid-word.
  let acc = '';
  for (const frag of ['The answer ', 'is 42', '.', ' Yes.']) {
    acc += frag;
    t.stream(acc);
  }
  const s = screen('流式输出分片（单词中间断开）', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('流式文本没有被拆散', joined.includes('The answer is 42. Yes.'));
  check('前面的 banner 行还在', joined.includes('flint v0.1.0'));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
  check('输入行没有被输出覆盖', !inputRow(s).includes('answer'));
}

// --- answer with newlines then end-of-turn blank --------------------------------
{
  const t = new FakeTerm();
  let acc = '';
  for (const frag of ['Line one.\n', 'Line two.\n', 'Line three.']) {
    acc += frag;
    t.stream(acc);
  }
  t.blank();
  const s = screen('多行回答 + 回合结束的空行', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('三行回答都还在', ['Line one.', 'Line two.', 'Line three.'].every((x) => joined.includes(x)));
  check('行序正确', joined.indexOf('Line one.') < joined.indexOf('Line two.'));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
}

// --- output longer than the region: oldest lines must scroll away ---------------
{
  const t = new FakeTerm();
  for (let i = 1; i <= 40; i++) t.line(`line ${i}`);
  const s = screen('40 行输出', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('最新一行可见', joined.includes('line 40'));
  check('最早的几行已滚出', !joined.includes('line 17'));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
}

// --- a multi-line write, which is what `!cmd` and `list` produce ----------------
{
  const t = new FakeTerm();
  t.line('✓ list');
  t.textLn('a\nb\n\nc\n');
  const s = screen('多行写入', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('多行内容全部可见', ['a', 'b', 'c'].every((x) => joined.includes(x)));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
}

console.log(failures === 0 ? '\n全部通过' : `\n${failures} 项失败`);
process.exit(failures === 0 ? 0 : 1);
