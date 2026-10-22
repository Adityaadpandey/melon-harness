import subprocess

from utils import search


def test_generated_folders_are_not_listed_and_a_listing_is_reused_until_a_write(tmp_path):
    subprocess.run(["git", "init", "-q"], cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x")
    (tmp_path / "graphify-out").mkdir()
    (tmp_path / "graphify-out" / "graph.json").write_text("{}")
    search.forget_listing()
    assert [p.name for p in search.list_files(tmp_path)] == ["a.ts"]
    (tmp_path / "b.ts").write_text("y")
    assert len(search.list_files(tmp_path)) == 2  # outside an agent round nothing is reused
    search.hold_listing(True)
    try:
        assert len(search.list_files(tmp_path)) == 2
        (tmp_path / "c.ts").write_text("z")
        assert len(search.list_files(tmp_path)) == 2  # same round: the first listing is reused
        search.forget_listing()  # the registry does this after any write
        assert len(search.list_files(tmp_path)) == 3
    finally:
        search.hold_listing(False)
