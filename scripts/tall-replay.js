// Show the *whole* transcript a run produced, not just the last screen.
//
// A real session scrolls its earlier lines off the top, so replaying onto a normal
// 24-row screen hides exactly the part under inspection -- the tool lines near the
// start of a turn. Replaying onto a tall screen keeps everything in one view.
//
// Usage: node scripts/tall-replay.js <raw-file> [cols]
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const file = process.argv[2];
const COLS = parseInt(process.argv[3] || '100', 10);
const ROWS = parseInt(process.argv[4] || '200', 10);

// Anything still on screen at the end is the visible transcript; the rest has
// scrolled away and is not worth printing.
const out = execFileSync('node', ['scripts/vtscreen.js', file, String(ROWS), String(COLS)], {
  encoding: 'utf8',
});
const lines = out.trimEnd().split('\n').slice(1);
console.log(`--- whole transcript (${path.basename(file)}) ---`);
console.log(
  lines
    .filter((l) => l.slice(3).trim() !== '')
    .join('\n')
);
