import pytest


@pytest.fixture(autouse=True)
def _private_data_dir(tmp_path_factory, monkeypatch):
    """Tests write sessions, traces and lessons into a temporary folder, never the real
    ~/Library/Application Support/melon."""
    import config.loader as loader

    base = tmp_path_factory.mktemp("appdirs")
    (base / "data" / "melon").mkdir(parents=True)
    (base / "data" / "melon" / "models.json").write_text("[]")  # a fresh, empty model catalog: no test fetches the real one
    monkeypatch.setattr(loader, "user_data_dir", lambda name: str(base / "data" / name))
    monkeypatch.setattr(loader, "user_config_dir", lambda name: str(base / "config" / name))


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
