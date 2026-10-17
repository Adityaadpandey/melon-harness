"""Each kind of request pulls in the playbook(s) for it, and mixed requests pull in several."""
from agent.skills import BUILTIN_DIR, match_skills, parse_skill

SKILLS = [parse_skill(f.read_text(), f) for f in sorted(BUILTIN_DIR.glob("*.md"))]


def names(request: str) -> list[str]:
    return [s.name for s in match_skills([s for s in SKILLS if s], request)]


def test_one_request_one_lens():
    assert "review-pr" in names("review pull request 42")
    assert "review-commits" in names("review the last commit")
    assert "review-changes" in names("look over my changes before I commit")
    assert "bug-hunt" in names("find bugs in the checkout flow")
    assert "scalability" in names("will this scale to a million users")
    assert "feature-planning" in names("how should we add a notifications feature")
    assert "performance" in names("the dashboard is slow, profile it")
    assert "security" in names("check the upload endpoint for vulnerabilities")


def test_mixed_request_loads_every_lens():
    got = names("review this pull request for security and performance")
    assert {"review-pr", "security", "performance"} <= set(got)


def test_review_commits_beats_the_bare_word_commit():
    got = names("review the last commit")
    assert got.index("review-commits") < (got.index("git-workflow") if "git-workflow" in got else 99)


def test_plain_chat_loads_nothing():
    assert names("hello there") == []


def test_repairing_can_repeat():
    from agent.task import _ALLOWED, TaskStatus
    assert TaskStatus.REPAIRING in _ALLOWED[TaskStatus.REPAIRING]


def test_shift_tab_cycles_modes_and_skips_yolo():
    from config.config import ApprovalPolicy as A
    from main import CLI

    order = CLI.MODES
    assert A.YOLO not in order and A.NEVER not in order
    assert order[0] == A.ON_REQUEST and A.PLAN in order
