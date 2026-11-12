"""Records the hero demo: runs the real zerd binary in a pseudo-terminal on a small demo repo and saves
what it prints, with real timing, as an asciinema v2 .cast. Usage: python3 record-demo.py out.cast

It types the prompt like a person, waits for the answer, then types /verify and waits for the checks.
Set ZERD_BIN, DEMO_REPO and DEMO_MODEL to change what is recorded. Before publishing a recording, scan it
for personal paths (it prints the model and a ~ path, but bash commands show absolute paths)."""
import fcntl, json, os, pty, select, signal, struct, sys, termios, time

COLS, ROWS = 112, 34
CWD = os.path.expanduser(os.environ.get("DEMO_REPO", "~/zerd-demo/retry-lab"))
BIN = os.environ.get("ZERD_BIN", "zerd")
MODEL = os.environ.get("DEMO_MODEL", "MiniMaxAI/MiniMax-M3")
# the word to wait for in the first screen, so we only type once the interface has drawn
WAIT = os.environ.get("DEMO_WAIT", MODEL.split("-")[0].split("/")[-1])
PROMPT = "send_with_retry never waits between tries after a 429. fix it so it backs off and respects retry_after"
OUT = sys.argv[1]
MAX_SECONDS = 260

pid, fd = pty.fork()
if pid == 0:
    os.chdir(CWD)
    os.environ.update(TERM="xterm-256color", COLORTERM="truecolor", COLUMNS=str(COLS), LINES=str(ROWS))
    os.execv(BIN, [BIN, "-m", MODEL, "--approval", "auto"])

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
start = time.time()
events, text = [], ""
last_out = time.time()


def pump(wait):
    """Read whatever the program printed within `wait` seconds."""
    global text, last_out
    end = time.time() + wait
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return False
            if not data:
                return False
            s = data.decode("utf-8", "replace")
            events.append([round(time.time() - start, 3), "o", s])
            text += s
            last_out = time.time()
    return True


# wait until the interface has really drawn before typing anything
t0 = time.time()
while WAIT not in text and time.time() - t0 < 40:
    pump(0.25)
pump(2.0)
for ch in PROMPT:  # type it the way a person would
    os.write(fd, ch.encode())
    pump(0.045)
pump(0.6)
os.write(fd, b"\r")

alive = True
sent = False
t_answer = None  # when the answer finished
t_sent = None    # when /verify was typed
while alive and time.time() - start < MAX_SECONDS:
    alive = pump(1.0)
    if not sent and "Worked for" in text:
        t_answer = t_answer or time.time()
        if time.time() - t_answer > 4:
            for ch in "/verify":
                os.write(fd, ch.encode())
                pump(0.08)
            pump(0.6)
            os.write(fd, bytes([13]))
            sent, t_sent = True, time.time()
            print("sent /verify at", round(t_sent - start, 1))
    elif sent and time.time() - t_sent > 25:
        break
pump(2.0)
try:
    os.kill(pid, signal.SIGTERM)
except ProcessLookupError:
    pass

with open(OUT, "w") as f:
    f.write(json.dumps({"version": 2, "width": COLS, "height": ROWS, "timestamp": int(start), "env": {"TERM": "xterm-256color"}, "title": f"zerd on {MODEL}"}) + "\n")
    for e in events:
        f.write(json.dumps(e) + "\n")
print("events", len(events), "seconds", round(time.time() - start, 1), "verified" in text)
