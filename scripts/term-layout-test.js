// Replay the exact escape sequences `Term` emits, and check what the user ends up
// seeing.
//
// This file mirrors src/term.rs, which follows Codex's inline-viewport model:
//
//   history region  rows 1..V-1. History is inserted here by narrowing the scroll
//                   region to those rows and emitting Reverse Index, so the strip
//                   below never moves.
//   answer strip    rows V..V+3 (the "viewport"). The streamed answer is redrawn
//                   into it in full on every fragment. Nothing here can disturb
//                   the transcript above.
//   input row       the last row on screen.
//
// Keeping the bytes in one place means a change in term.rs that breaks the layout
// fails here instead of on someone's screen.
//
// Usage: node scripts/term-layout-test.js
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ESC = '\x1b';
const ROWS = 24;
const COLS = 70;
const ANSWER_ROWS = 4;
const INPUT = ROWS; // last row
const VIEWPORT_TOP = ROWS - ANSWER_ROWS; // 20
const HISTORY_BOTTOM = VIEWPORT_TOP - 1; // 19

const tmp = path.join(os.tmpdir(), 'term-layout.bin');

class FakeTerm {
  constructor() {
    this.s = `${ESC}[1;${ROWS}r${ESC}[1;1H`;
    this.viewportTop = VIEWPORT_TOP;
    this.streamActive = false;
    this.streamText = '';
    this.committed = 0;
  }
  redraw(prefix = '> ') {
    this.s += `${ESC}[${INPUT};1H${ESC}[2K${ESC}[1m${prefix}${ESC}[0m`;
  }
  // Mirrors Term::insert_history.
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
  emitHistory(text) {
    this.insertHistory([text]);
  }
  line(text) {
    this.closeStream();
    this.emitHistory(text);
  }
  blank() {
    this.line('');
  }
  textLn(text) {
    for (const part of text.replace(/[\r\n]+$/, '').split('\n')) {
      this.line(part.replace(/\r$/, ''));
    }
  }
  endStream() {
    this.closeStream();
  }
  // Mirrors Term::scroll_history_down.
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
    const left = lines.slice(lines.length - shown);
    this.insertHistory(left);
    for (let r = top; r <= last; r++) this.s += `${ESC}[${r};1H${ESC}[2K`;
    this.s += `${ESC}[${top};${last}r${ESC}[${top};1H`;
    for (let i = 0; i < shown; i++) this.s += '\r\n';
    this.s += `${ESC}[r`;
    this.committed = 0;
    // A blank line separates the answer from whatever comes next.
    this.emitHistory('');
  }
  // Mirrors Term::stream.
  stream(text) {
    this.streamActive = true;
    this.streamText = text;
    const rows = text.split('\n');
    const height = rows.length;
    const capacity = INPUT - 1 - VIEWPORT_TOP + 1;
    if (height > capacity) {
      const drop = height - capacity;
      if (drop > this.committed) {
        this.insertHistory(rows.slice(this.committed, drop));
        this.committed = drop;
      }
    }
    const first = Math.max(0, height - capacity);
    const visible = rows.slice(first);
    const start = VIEWPORT_TOP + (capacity - visible.length);
    let cursor = null;
    for (let n = 0; n < visible.length; n++) {
      const r = start + n;
      if (r > INPUT - 1) break;
      this.s += `${ESC}[${r};1H${ESC}[2K${visible[n]}`;
      cursor = [r, [...visible[n]].length];
    }
    if (cursor) this.s += `${ESC}[${cursor[0]};${Math.max(cursor[1], 1)}H`;
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

function inputRow(s) {
  return s[s.length - 1];
}

// --- the reported bug: the question scrolled away -------------------------------
{
  const t = new FakeTerm();
  t.line('flint v0.1.0  deepseek/deepseek-flash');
  t.blank();
  t.line('> 你好，帮我看看磁盘');
  let acc = '';
  for (const frag of ['正在', '检查', '磁盘', '。']) {
    acc += frag;
    t.stream(acc);
  }
  t.endStream();
  const s = screen('用户消息必须留在屏幕上', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('用户的话还在', joined.includes('你好，帮我看看磁盘'));
  check('模型的回答也在', joined.includes('正在检查磁盘。'));
  check('回答在提问下方', joined.indexOf('你好') < joined.indexOf('正在检查'));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
  check('回答没有盖住输入行', !inputRow(s).includes('正在检查'));
}

// --- streamed fragments split mid-word ------------------------------------------
{
  const t = new FakeTerm();
  t.line('> question');
  let acc = '';
  for (const frag of ['The answer ', 'is 42', '.', ' Yes.']) {
    acc += frag;
    t.stream(acc);
  }
  t.endStream();
  const s = screen('流式分片（单词中间断开）', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('流式文本没有被拆散', joined.includes('The answer is 42. Yes.'));
  check('提问还在', joined.includes('> question'));
  check('回答只出现一次', joined.split('The answer is 42. Yes.').length === 2);
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
}

// --- the reported bug: reasoning must not be left stranded ----------------------
{
  const t = new FakeTerm();
  t.line('> 问题');
  let reasoning = '';
  for (const frag of ['让我想想这个问题的', '关键点在哪里，', '可能需要先确认一些事。']) {
    reasoning += frag;
    t.stream(reasoning);
  }
  t.endStream();
  let acc = '';
  for (const frag of ['好的', '。']) {
    acc += frag;
    t.stream(acc);
  }
  t.endStream();
  const s = screen('短回答不能留下长思考的残尾', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('回答可见', joined.includes('好的。'));
  check('思考完整保留，没有被回答截断', joined.includes('可能需要先确认一些事。'));
  check('回答在思考之后', joined.indexOf('可能需要先确认') < joined.indexOf('好的。'));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
}

// --- a tool result mid-turn must not land on the answer -------------------------
{
  const t = new FakeTerm();
  t.line('> 跑一下命令');
  t.stream('我先看看。');
  t.line('  ✓ TOOL_ROUND_OK');
  t.stream('我先看看。输出是 TOOL_ROUND_OK。');
  t.endStream();
  const s = screen('工具结果不能盖住回答', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('工具结果可见', joined.includes('✓ TOOL_ROUND_OK'));
  check('最终回答可见', joined.includes('输出是 TOOL_ROUND_OK。'));
  check('提问可见', joined.includes('跑一下命令'));
  check('输入行没有被答案占用', !inputRow(s).includes('TOOL_ROUND_OK'));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
}

// --- output longer than the screen ----------------------------------------------
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

// --- multi-line write, which is what `!cmd` and `list` produce ------------------
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

// --- an answer longer than the strip: it must grow, not vanish ------------------
{
  const t = new FakeTerm();
  t.line('> 讲故事');
  let acc = '';
  for (let i = 1; i <= 8; i++) {
    acc += (i > 1 ? '\n' : '') + `第 ${i} 行回答`;
    t.stream(acc);
  }
  t.endStream();
  const s = screen('回答超过 4 行时视口不移动', () => t);
  const joined = s.join('\n');
  console.log('  --- assertions ---');
  check('最后几行仍然可见', joined.includes('第 8 行回答'));
  check('提问没有被回答挤掉', joined.includes('> 讲故事'));
  check('输入行固定在最后一行', inputRow(s).startsWith('>'));
  check('视口没有被内容溢出', !inputRow(s).includes('第'));
  check('滚出视口的行也没有丢', joined.includes('第 1 行回答'));
  check('八行齐全', [1, 2, 3, 4, 5, 6, 7, 8].every((i) => joined.includes(`第 ${i} 行回答`)));
}

// --- the replay model itself -----------------------------------------------------
// Everything above is only as trustworthy as the width model underneath it. A
// streamed Chinese answer was reported as duplicating its first row, which turned
// out to be the replay treating each CJK character as one column; the code was
// fixed, but the check belongs here too, or a wrong emulator can hide a real bug.
{
  const wide = '你'.repeat(60); // 120 columns of CJK
  const narrow = 'a'.repeat(60); // 60 columns of ASCII
  fs.writeFileSync(tmp, `${wide}\r\n${narrow}`, 'utf8');
  const out = execFileSync('node', ['scripts/vtscreen.js', tmp, '24', String(COLS)], {
    encoding: 'utf8',
  });
  const lines = out.trimEnd().split('\n').slice(1).map((l) => l.slice(3).replace(/\s+$/, ''));
  console.log('\n===== 回放器宽度模型 =====');
  console.log(`  row1=${[...lines[0]].length} chars  row2=${[...lines[1]].length} chars`);
  console.log('  --- assertions ---');
  check('120 列的中文在 70 列处换行', [...lines[0]].length === 35);
  check('换行后余下的中文在下一行', [...lines[1]].length === 25);
  check('60 列 ASCII 不换行', lines[2].length === 60);
}
// The hand-written model above is only worth anything if it matches what `Term`
// actually emits. `cargo test --test term_capture` writes that stream to
// target/term-capture.bin; replaying it here is what ties the two together.
{
  const capturePath = path.join(__dirname, '..', 'target', 'term-capture.bin');
  if (!fs.existsSync(capturePath)) {
    console.log('\n===== 真实字节回放 =====');
    console.log('  SKIP  没有 target/term-capture.bin（先跑 cargo test --test term_capture）');
  } else {
    fs.copyFileSync(capturePath, tmp);
    const out = execFileSync('node', ['scripts/vtscreen.js', tmp, String(ROWS), String(COLS)], {
      encoding: 'utf8',
    });
    const lines = out.trimEnd().split('\n').slice(1);
    console.log('\n===== 真实字节回放 =====');
    console.log(lines.join('\n'));
    const s = lines.map((l) => l.slice(3).replace(/\s+$/, ''));
    const joined = s.join('\n');
    console.log('  --- assertions ---');
    check('真字节里用户的话还在', joined.includes('你好，帮我看看磁盘'));
    check('真字节里思考只占一行', joined.includes('… thinking'));
    check('真字节里思考没有铺满屏幕', joined.split('thinking').length === 2);
    check('真字节里工具结果在', joined.includes('TOOL_ROUND_OK'));
    check('真字节里回答完整', joined.includes('磁盘占用正常。'));
    check('真字节里输入行固定在最后一行', inputRow(s).startsWith('>'));
    check('真字节里回答没有盖住输入行', !inputRow(s).includes('磁盘占用正常'));
  }
}

console.log(failures === 0 ? '\n全部通过' : `\n${failures} 项失败`);
process.exit(failures === 0 ? 0 : 1);
