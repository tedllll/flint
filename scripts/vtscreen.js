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

/// Display width of a code point.
///
/// This matters more than it looks. flint prints file contents, and the `read`
/// tool decorates them with box-drawing rules and line numbers; CJK answers are
/// routine. Treating every code point as one column makes the model's column
/// arithmetic disagree with a real terminal, and the replay then shows wrapping
/// that never happened. So: the standard East Asian wide and fullwidth ranges are
/// two columns, everything else one.
function charWidth(cp) {
  const c = cp.codePointAt(0);
  if (c === undefined) return 1;
  // Combining marks and control characters take no room.
  if ((c >= 0x0300 && c <= 0x036f) || c === 0x200b || c === 0xfeff) return 0;
  const wide =
    (c >= 0x1100 && c <= 0x115f) || // Hangul Jamo
    (c >= 0x2e80 && c <= 0x303e) || // CJK radicals, punctuation
    (c >= 0x3041 && c <= 0x33ff) || // kana, CJK compatibility
    (c >= 0x3400 && c <= 0x4dbf) || // CJK extension A
    (c >= 0x4e00 && c <= 0x9fff) || // CJK unified ideographs
    (c >= 0xa000 && c <= 0xa4cf) || // Yi
    (c >= 0xac00 && c <= 0xd7a3) || // Hangul syllables
    (c >= 0xf900 && c <= 0xfaff) || // CJK compatibility ideographs
    (c >= 0xfe30 && c <= 0xfe6f) || // CJK compatibility forms
    (c >= 0xff00 && c <= 0xff60) || // fullwidth forms
    (c >= 0xffe0 && c <= 0xffe6) ||
    (c >= 0x1f300 && c <= 0x1f64f) || // emoji
    (c >= 0x20000 && c <= 0x3fffd); // CJK extensions B+
  return wide ? 2 : 1;
}

// VT100 "pending wrap" state.
//
// Writing a character into the last column does NOT move the cursor to the next
// row; the terminal remembers that the next character must wrap first. The
// difference is visible: flint parks the cursor with an absolute position, and
// under pending-wrap semantics a `CR` at that point clears column 0 of the row it
// is already on, not of the row below. Getting this wrong made a correct stream
// look like it repeated the answer first row.
let pendingWrap = false;

function put(ch) {
  const width = charWidth(ch);
  if (width === 0) return;
  if (pendingWrap) {
    col = 0;
    newline();
    pendingWrap = false;
  }
  if (col + width > COLS) {
    // Autowrap, but never split a wide cell across the boundary.
    col = 0;
    newline();
  }
  screen[row][col] = ch;
  for (let k = 1; k < width; k++) screen[row][col + k] = '';
  col += width;
  if (col >= COLS) {
    col = COLS - 1;
    pendingWrap = true;
  }
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
        pendingWrap = false;
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
    pendingWrap = false;
    i++;
    continue;
  }
  if (b === 0x0a) {
    newline();
    i++;
    continue;
  }
  if (b === 0x09) {
    // Tab. The `read` tool separates its line-number column with a tab, so this is
    // load-bearing: treating a tab as zero-width scatters the numbers across the
    // line and makes correct output look broken.
    col = Math.min(COLS - 1, (Math.floor(col / 8) + 1) * 8);
    i++;
    continue;
  }
  if (b === 0x08) {
    col = Math.max(0, col - 1);
    pendingWrap = false;
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
  put(cp);
  i += len;
}

console.log(`--- screen ${ROWS}x${COLS}, scroll region rows ${top + 1}..${bottom + 1} ---`);
screen.forEach((r, n) => {
  console.log(String(n + 1).padStart(2) + '|' + r.join('').replace(/\s+$/, ''));
});
