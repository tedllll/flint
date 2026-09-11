// Drives flint over a real pipe so a mid-turn interrupt can be exercised.
//
// A redirected *file* does not work: reads return EOF at the end of the file
// instead of blocking for more input, which is not what a terminal does. A pipe
// held open by this process does block, so writing to it later looks exactly
// like someone typing partway through a turn.
//
// stdio is inherited rather than piped, because the harness sandbox forbids
// capturing another process's output through a pipe.
const { spawn } = require("node:child_process");
const path = require("node:path");

const exe = path.join(process.env.USERPROFILE, "bin", "flint.exe");
const child = spawn(exe, ["--no-color"], { stdio: ["pipe", "inherit", "inherit"] });

const slow =
  "Write a long, detailed 800-word essay about the Rust ownership model. " +
  "Think carefully between paragraphs.\n";
const steer = "Stop. Do not write the essay. Reply with exactly: INTERRUPT-OK\n";

process.stderr.write(`[test] pid=${child.pid}\n`);
child.stdin.write(slow);

setTimeout(() => {
  process.stderr.write("[test] --- sending interrupt ---\n");
  child.stdin.write(steer);
}, 3000);

setTimeout(() => {
  child.stdin.write("/exit\n");
}, 25000);

setTimeout(() => {
  process.stderr.write("[test] timeout; killing\n");
  child.kill();
  process.exit(1);
}, 60000);

child.on("exit", (code) => {
  process.stderr.write(`[test] flint exited with ${code}\n`);
  process.exit(0);
});
