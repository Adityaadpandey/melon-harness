import pytest


@pytest.fixture(autouse=True)
def _no_personal_skills(request, monkeypatch):
    """Tests must not depend on the skills installed on this machine (~/.claude, ~/.pi).
    A test that wants them marks itself with @pytest.mark.real_skill_dirs."""
    from agent import skills

    skills._load.cache_clear()
    if "real_skill_dirs" not in request.keywords:
        monkeypatch.setattr(skills, "_user_dirs", lambda: ([], []))
    yield
    skills._load.cache_clear()


def pytest_configure(config):
    config.addinivalue_line("markers", "real_skill_dirs: let the test see the skill folders under (a fake) home")
