// Replay a raw terminal byte stream through a small VT100 model and print the
// resulting screen.
//
// This exists because the output path cannot be judged by eye: escape sequences
// either put text where the user can see it or they do not, and that is a
// deterministic question. Reading raw bytes back through a screen model answers
// it without a terminal, a human, or a rebuild.
//
// Modelled, because these are what flint actually emits:
//   CSI r    set scrolling region (DECSTBM)
//   CSI H    cursor position
//   CSI A/B  cursor up/down
//   CSI K    erase in line
//   CSI ?25l/h  hide/show cursor
//
// Usage: node scripts/vtscreen.js <raw-file> [rows] [cols]
const fs = require('fs');

const file = process.argv[2];
const ROWS = parseInt(process.argv[3] || '24', 10);
const COLS = parseInt(process.argv[4] || '80', 10);

const screen = Array.from({ length: ROWS }, () => Array(COLS).fill(' '));
let row = 0; // 0-based
let col = 0;
let top = 0; // scroll region, 0-based inclusive
let bottom = ROWS - 1;

function scrollUp() {
  screen.splice(top, 1);
  screen.splice(bottom, 0, Array(COLS).fill(' '));
}

function scrollDown() {
  screen.splice(bottom, 1);
  screen.splice(top, 0, Array(COLS).fill(' '));
}

/// Reverse Index: move up one row, scrolling the region down if already at its top.
/// If the cursor is above the region's top, it simply moves up.
function reverseIndex() {
  if (row > top && row > 0) {
    row--;
  } else if (row === top) {
    scrollDown();
  } else if (row > 0) {
    row--;
  }
}

function put(ch) {
  if (col >= COLS) {
    // Autowrap.
    col = 0;
    newline();
  }
  screen[row][col] = ch;
  col++;
}

function newline() {
  if (row === bottom) {
    scrollUp();
  } else if (row < ROWS - 1) {
    row++;
  }
}

const bytes = fs.readFileSync(file);
let i = 0;
while (i < bytes.length) {
  const b = bytes[i];

  if (b === 0x1b && bytes[i + 1] === 0x4d) {
    // ESC M -- Reverse Index, which is what inserts history above the viewport.
    reverseIndex();
    i += 2;
    continue;
  }

  if (b === 0x1b && bytes[i + 1] === 0x5b) {
    // CSI
    let j = i + 2;
    let params = '';
    while (j < bytes.length && !(bytes[j] >= 0x40 && bytes[j] <= 0x7e)) {
      params += String.fromCharCode(bytes[j]);
      j++;
    }
    const final = String.fromCharCode(bytes[j]);
    const nums = params
      .replace(/^[?]/, '')
      .split(';')
      .map((x) => (x === '' ? null : parseInt(x, 10)));

    switch (final) {
      case 'r': {
        top = (nums[0] || 1) - 1;
        bottom = (nums[1] || ROWS) - 1;
        break;
      }
      case 'H': {
        row = Math.min((nums[0] || 1) - 1, ROWS - 1);
        col = Math.min((nums[1] || 1) - 1, COLS - 1);
        break;
      }
      case 'A':
        row = Math.max(top, row - (nums[0] || 1));
        break;
      case 'B':
        row = Math.min(bottom, row + (nums[0] || 1));
        break;
      case 'C':
        col = Math.min(COLS - 1, col + (nums[0] || 1));
        break;
      case 'D':
        col = Math.max(0, col - (nums[0] || 1));
        break;
      case 'S': {
        // Scroll the region up by n rows.
        const n = nums[0] || 1;
        for (let k = 0; k < n; k++) scrollUp();
        break;
      }
      case 'K': {
        const mode = nums[0] || 0;
        if (mode === 0) for (let c = col; c < COLS; c++) screen[row][c] = ' ';
        else if (mode === 1) for (let c = 0; c <= col; c++) screen[row][c] = ' ';
        else screen[row] = Array(COLS).fill(' ');
        break;
      }
      case 'J': {
        const mode = nums[0] || 0;
        if (mode === 0) {
          for (let c = col; c < COLS; c++) screen[row][c] = ' ';
          for (let r = row + 1; r < ROWS; r++) screen[r] = Array(COLS).fill(' ');
        } else if (mode === 2) {
          for (let r = 0; r < ROWS; r++) screen[r] = Array(COLS).fill(' ');
        }
        break;
      }
      default:
        break; // ?25l / ?25h and anything else: no screen effect
    }
    i = j + 1;
    continue;
  }

  if (b === 0x0d) {
    col = 0;
    i++;
    continue;
  }
  if (b === 0x0a) {
    newline();
    i++;
    continue;
  }
  if (b === 0x08) {
    col = Math.max(0, col - 1);
    i++;
    continue;
  }
  if (b < 0x20) {
    i++;
    continue;
  }

  // UTF-8 aware: collect a whole code point.
  let len = 1;
  if (b >= 0xf0) len = 4;
  else if (b >= 0xe0) len = 3;
  else if (b >= 0xc0) len = 2;
  const cp = bytes.slice(i, i + len).toString('utf8');
  // Width: treat box-drawing and CJK as narrow here; good enough for layout.
  put(cp);
  i += len;
}

console.log(`--- screen ${ROWS}x${COLS}, scroll region rows ${top + 1}..${bottom + 1} ---`);
screen.forEach((r, n) => {
  console.log(String(n + 1).padStart(2) + '|' + r.join('').replace(/\s+$/, ''));
});
