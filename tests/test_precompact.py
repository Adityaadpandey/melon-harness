from utils.precompact import pre_compact


def test_ansi_progress_and_repeats_are_dropped_but_the_error_stays():
    bars = "\n".join(f"Downloading  {i}%|{'#' * (i // 5)}" for i in range(1, 100))
    lint = "\n".join(f"src/a.ts:{i}:1 error no-unused-vars 'x{i}' is defined" for i in range(40))
    out = pre_compact("\x1b[31mFAILED\x1b[0m\n" + bars + "\n" + lint + "\nAssertionError: boom")
    assert "\x1b" not in out and "FAILED" in out and "AssertionError: boom" in out
    assert out.count("Downloading") == 1 and out.count("no-unused-vars") == 3
    assert len(out) < 0.2 * (len(bars) + len(lint))


def test_long_python_traceback_keeps_both_ends():
    frames = "".join(f'  File "m{i}.py", line {i}, in f{i}\n    call{i}()\n' for i in range(30))
    out = pre_compact("Traceback (most recent call last):\n" + frames + "ValueError: bad " + "x" * 400)
    assert 'File "m0.py"' in out and 'File "m29.py"' in out and 'File "m15.py"' not in out and "ValueError: bad" in out


def test_short_text_is_untouched():
    assert pre_compact("ok\n\x1b[1mdone") == "ok\n\x1b[1mdone"
