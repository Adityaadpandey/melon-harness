from pathlib import Path

from safety.rules import Rules, drop_rule, load_rules, parse_rule, save_rule


def rules(tmp_path, **kinds):
    return Rules([parse_rule(t, k) for k, ts in kinds.items() for t in ts], tmp_path)


def test_bash_prefix_and_exact(tmp_path):
    r = rules(tmp_path, allow=["Bash(git diff:*)", "Bash(npm run build)"])
    assert r.decide("shell", {"command": "git diff HEAD~1"})
    assert r.decide("shell", {"command": "npm run build"})
    assert not r.decide("shell", {"command": "npm run build --watch"})
    assert not r.decide("shell", {"command": "git difftool"})  # a word boundary, not a raw prefix


def test_allow_needs_every_piece_deny_needs_any(tmp_path):
    assert not rules(tmp_path, allow=["Bash(ls:*)"]).decide("shell", {"command": "ls && rm -r x"})
    assert rules(tmp_path, deny=["Bash(rm:*)"]).decide("shell", {"command": "ls && rm -r x"})


def test_deny_beats_ask_beats_allow(tmp_path):
    r = rules(tmp_path, allow=["Bash"], ask=["Bash(git push:*)"], deny=["Bash(git push --force:*)"])
    assert r.decide("shell", {"command": "git push --force origin"}).kind == "deny"
    assert r.decide("shell", {"command": "git push origin"}).kind == "ask"
    assert r.decide("shell", {"command": "ls"}).kind == "allow"


def test_path_patterns(tmp_path):
    r = rules(tmp_path, deny=["Read(.env*)", "Edit(infra/**)"], allow=["Edit(src/**)"])
    assert r.decide("read_file", {"path": ".env.local"}).kind == "deny"
    assert r.decide("grep", {"path": "apps/web/.env"}).kind == "deny"  # a bare name matches at any depth
    assert r.decide("write_file", {"path": "infra/main.tf"}).kind == "deny"
    assert r.decide("edit", {"path": str(tmp_path / "src" / "a" / "b.ts")}).kind == "allow"
    assert not r.decide("edit", {"path": "docs/readme.md"})


def test_webfetch_domain(tmp_path):
    r = rules(tmp_path, allow=["WebFetch(domain:python.org)"])
    assert r.decide("web_fetch", {"url": "https://docs.python.org/3/"})
    assert not r.decide("web_fetch", {"url": "https://evilpython.org/"})


def test_files_roundtrip_and_claude_settings(tmp_path):
    save_rule(tmp_path, "deny", "Read(.env*)")
    (tmp_path / ".claude").mkdir()
    (tmp_path / ".claude" / "settings.json").write_text('{"permissions": {"allow": ["Bash(make:*)"]}}')
    r = load_rules(tmp_path)
    assert r.decide("shell", {"command": "make test"}).kind == "allow"
    assert r.decide("read_file", {"path": ".env"}).kind == "deny"
    assert drop_rule(tmp_path, "Read(.env*)") and not load_rules(tmp_path).decide("read_file", {"path": ".env"})
