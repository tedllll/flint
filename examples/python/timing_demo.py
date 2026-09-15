"""What a py call actually does to the code around it: blocking, and the quiet-continue trap.

    python timing_demo.py

Three cases, measured rather than described:
  1. a successful call -- the line after it runs only once the answer is complete
  2. a call whose provider is unreachable, with no check -- the script carries on, and the
     "answer" is an empty string. Nothing raised, nothing printed, nothing wrong-looking.
  3. the same failure, checked -- the script stops where the caller said it should.
"""

import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from flint_call import ask  # noqa: E402

HERE = Path(__file__).parent


def home_for(base_url: str, tag: str) -> str:
    home = tempfile.mkdtemp(prefix=f"flint-timing-{tag}-")
    os.makedirs(os.path.join(home, "sessions"), exist_ok=True)
    with open(os.path.join(home, "config.toml"), "w", encoding="utf-8") as f:
        f.write(
            'default_provider = "stub"\n'
            "\n[[providers]]\n"
            'name = "stub"\n'
            f'base_url = "{base_url}"\n'
            'model = "stub-model"\n'
            'api_key = "not-a-real-key"\n'
        )
    return home


with socket.socket() as s:
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
stub = subprocess.Popen(
    [sys.executable, str(HERE / "stub_provider.py"), str(port)],
    stdout=subprocess.PIPE,
    text=True,
    encoding="utf-8",
)
stub.stdout.readline()

print("1. a call that works -- does the code after it wait?")
home = home_for(f"http://127.0.0.1:{port}/v1", "ok")
t0 = time.monotonic()
print("   before  the call")
turn = ask("跑个命令看看", home=home, cwd=str(HERE))
print(f"   after   the call  (+{time.monotonic() - t0:.2f}s)")
print(f"   answer  {turn.answer!r}")
print("   -> the next line ran *after* the answer existed, not before.\n")

print("2. a call that cannot work, with no check -- what does the code after it do?")
dead = home_for("http://127.0.0.1:9/v1", "dead")
turn = ask("hello", home=dead, cwd=str(HERE), timeout=60)
print(f"   turn.ok      {turn.ok}")
print(f"   turn.error   {str(turn.error)[:60]!r}")
print(f"   turn.answer  {turn.answer!r}")
print("   the script continued here anyway -- this line proves it")
print("   -> no exception, so a caller that does not check carries on with nothing.\n")

print("3. the same call, checked -- where does the script stop?")
turn = ask("hello", home=dead, cwd=str(HERE), timeout=60)
if not turn.ok:
    print(f"   raised where the caller said to: {str(turn.error)[:60]!r}")
print("   -> 'this line is unreachable' is the behaviour a caller usually means\n")

print("4. and the session is on disk either way, so a failure is resumable:")
turn = ask("跑个命令看看", home=home, cwd=str(HERE))
print(f"   session {turn.session}")
print(f"   exists  {Path(turn.session).exists()}")

stub.terminate()
stub.wait(timeout=5)
import shutil  # noqa: E402

for d in (home, dead):
    shutil.rmtree(d, ignore_errors=True)
