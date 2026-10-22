from agent import review_pipeline as rp
from config.config import Config


def test_one_small_file_gets_one_reviewer():
    cfg = Config()
    assert [n for n, _ in rp.pick_lenses(cfg, "+x\n" * 20, ["a.yml"], "")] == ["review"]
    assert [n for n, _ in rp.pick_lenses(cfg, "+x\n" * 20, ["a.ts", "b.ts"], "")] == ["review", "contracts"]
