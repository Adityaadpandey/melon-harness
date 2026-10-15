import subprocess

from utils.repomap import build


def test_most_imported_file_comes_first(tmp_path):
    (tmp_path / "core.py").write_text("class Engine:\n    pass\n\ndef start():\n    pass\n")
    for n in "abc":
        (tmp_path / f"{n}.py").write_text(f"import core\n\ndef run_{n}():\n    pass\n")
    subprocess.run(["git", "init", "-q"], cwd=tmp_path)
    subprocess.run(["git", "add", "."], cwd=tmp_path)
    out = build(tmp_path).splitlines()
    assert out[0] == "core.py: Engine, start" and len(out) == 4


def test_budget_cuts_the_tail(tmp_path):
    for i in range(30):
        (tmp_path / f"m{i}.py").write_text("def f():\n    pass\n")
    subprocess.run(["git", "init", "-q"], cwd=tmp_path)
    subprocess.run(["git", "add", "."], cwd=tmp_path)
    assert len(build(tmp_path, budget_chars=100)) <= 100
