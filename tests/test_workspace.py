import os
import subprocess
from pathlib import Path
import sys

import pytest

from config.config import ApprovalPolicy, Config
from safety import sandbox
from safety.guard import violation
from safety.workspace import Workspace
from utils.netguard import check_url


@pytest.fixture
def ws(tmp_path):
    root = tmp_path / "home" / "proj"  # a project that lives under a (fake) home dir, like ~/Devlopment/x
    root.mkdir(parents=True)
    (root / "src").mkdir()
    return root


@pytest.fixture(autouse=True)
def fake_home(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    monkeypatch.setenv("HOME", str(home))


def test_paths_inside_are_fine_outside_and_symlink_escapes_are_not(ws, tmp_path):
    w = Workspace(ws)
    assert w.contains("src/a.py") and w.contains(str(ws / "x")) and w.contains(".")
    assert not w.contains("../other") and not w.contains("/etc/passwd") and not w.contains("~/.ssh/id_rsa")
    secret_dir = tmp_path / "private"
    secret_dir.mkdir()
    (ws / "link").symlink_to(secret_dir)  # a link inside the workspace pointing out of it
    assert not w.contains("link/file.txt")
    assert Workspace(ws, [str(secret_dir)]).contains("link/file.txt")  # unless explicitly added


@pytest.mark.parametrize("cmd", [
    "cat ~/.ssh/id_rsa", "cat $HOME/.aws/credentials", "ls ${HOME}/Documents", "cd .. && ls", "cat ../secrets.txt",
    "cp src/a.py /Users/someone/else.py", "python -c \"open('/etc/passwd').read()\"", "ls ~", "cat /home/bob/.bashrc",
])
def test_shell_commands_cannot_name_private_paths_outside(ws, cmd):
    assert Workspace(ws).shell_violation(cmd), cmd


@pytest.mark.parametrize("cmd", [
    "ls src && cat src/a.py", "pnpm test", "cat /dev/null", "ls /usr/bin | head", "echo hi > /tmp/x.txt",
    "curl https://example.com/a/b", "git log --oneline", "cat ./src/../src/a.py", "node -e \"console.log(1)\"",
])
def test_ordinary_commands_are_not_flagged(ws, cmd):
    assert Workspace(ws).shell_violation(cmd) is None, cmd


def test_tool_paths_are_confined_through_the_guard(ws):
    c = Config(cwd=ws)
    assert violation("read_file", {"path": "src/a.py"}, c) is None
    assert "outside the workspace" in violation("read_file", {"path": "../../etc/passwd"}, c)
    assert "outside the workspace" in violation("shell", {"command": "ls", "cwd": "/"}, c)
    assert "outside the workspace" in violation("write_file", {"path": "/Users/x/evil.sh", "content": ""}, c)
    assert "protected pattern" in violation("read_file", {"path": ".env.production"}, c)  # secrets blocked by default
    assert violation("read_file", {"path": ".env.example"}, c) is None
    assert "outside the workspace" in violation("some_mcp_tool", {"file": "/Users/x/.ssh/id_rsa"}, c)
    assert violation("read_file", {"path": "/x/y"}, Config(cwd=ws, allowed_dirs=["/x"])) is None


async def test_registry_blocks_escape_attempts_end_to_end(ws, tmp_path):
    from agent.agent import Agent

    outside = tmp_path / "outside.txt"
    outside.write_text("top secret")
    async with Agent(Config(cwd=ws, approval=ApprovalPolicy.YOLO)) as a:
        r = await a.session.tool_registry.invoke("read_file", {"path": str(outside)}, ws, a.session.hook_system)
        assert not r.success and "outside the workspace" in r.error and "top secret" not in r.output
        r = await a.session.tool_registry.invoke("write_file", {"path": "../hacked.txt", "content": "x"}, ws, a.session.hook_system, a.session.approval_manager)
        assert not r.success and not (tmp_path / "hacked.txt").exists()


@pytest.mark.skipif(not sandbox.available(), reason="needs macOS sandbox-exec")
async def test_os_sandbox_blocks_writes_outside_and_secret_reads_even_when_text_checks_are_bypassed(ws, tmp_path, monkeypatch):
    real = Path(os.path.expanduser('~' + os.environ.get('USER', ''))).resolve()
    monkeypatch.setenv('HOME', str(real))  # the sandbox profile must be built from the real home
    cfg = Config(cwd=ws)

    def run(script):
        argv = sandbox.wrap(["/bin/bash", "-c", script], cfg)
        return subprocess.run(argv, cwd=ws, capture_output=True, text=True)

    assert run("echo ok > inside.txt").returncode == 0 and (ws / "inside.txt").exists()
    # built dynamically so no static check could see it; the real home is never writable from the sandbox
    probe = Path(os.path.realpath(os.environ.get("REAL_HOME", "/Users"))) / f"mew_sandbox_probe_{os.getpid()}"
    real_home = Path(os.path.expanduser("~" + os.environ.get("USER", ""))).resolve()
    victim = real_home / f"mew_sandbox_probe_{os.getpid()}.txt"
    r = run(f'h=$(eval echo ~{os.environ.get("USER", "")}); echo x > "$h/mew_sandbox_probe_{os.getpid()}.txt"')
    assert r.returncode != 0 and not victim.exists()
    victim.unlink(missing_ok=True)
    r = run(f"ls {real_home}/.ssh")
    assert r.returncode != 0
    assert run("echo t > /tmp/mew_sandbox_probe && rm /tmp/mew_sandbox_probe").returncode == 0
    assert run("python3 -c 'print(1)' && git --version").returncode == 0


def test_sandbox_off_and_unavailable_modes(ws, monkeypatch):
    assert sandbox.wrap(["/bin/true"], Config(cwd=ws, sandbox="off")) == ["/bin/true"]
    assert sandbox.mode(Config(cwd=ws, sandbox="off")) == "off"
    monkeypatch.setattr(sandbox, "available", lambda: False)
    assert sandbox.mode(Config(cwd=ws)) == "unavailable"


def test_network_guard_blocks_local_and_private_targets():
    assert check_url("http://localhost:3000/admin")
    assert check_url("http://127.0.0.1/")
    assert check_url("http://169.254.169.254/latest/meta-data/")
    assert check_url("http://192.168.1.10/")
    assert check_url("file:///etc/passwd")
    assert check_url("http://127.0.0.1/", allow_private=True) is None
    assert check_url("http://this-host-does-not-exist.invalid/") is None  # lets the request fail normally


async def test_fetch_refuses_redirect_to_localhost(monkeypatch):
    import httpx
    from tools.base import ToolInvocation
    from tools.builtin.web_fetch import WebFetchTool

    monkeypatch.setattr("tools.builtin.web_fetch.check_url", lambda url, allow=False: "private" if "127.0.0.1" in url else None)

    def handler(request):
        return httpx.Response(302, headers={"location": "http://127.0.0.1:8080/secret"})

    real = httpx.AsyncClient
    monkeypatch.setattr(httpx, "AsyncClient", lambda **kw: real(transport=httpx.MockTransport(handler), **kw))
    r = await WebFetchTool(Config(cwd=".")).execute(ToolInvocation(params={"url": "https://public.example/x"}, cwd=__import__("pathlib").Path(".")))
    assert not r.success and "Blocked" in r.error


@pytest.mark.parametrize("cmd", ["cd /; cat etc/passwd", "cd && cat .ssh/config", "cd ~ ; ls", "cd - ; ls", "if true; then cd /; fi"])
def test_a_shell_may_not_cd_out_of_the_workspace(ws, cmd):
    assert "leaves the workspace" in Workspace(ws).shell_violation(cmd), cmd


def test_cd_inside_the_workspace_or_to_temp_is_fine(ws):
    for cmd in ["cd src && ls", "cd /tmp && ls", "cd ./src; pwd"]:
        assert Workspace(ws).shell_violation(cmd) is None, cmd


def test_secret_files_are_protected_in_every_spelling_but_templates_stay_readable(ws):
    c = Config(cwd=ws)
    for cmd in ["git show HEAD:.env", "git show HEAD:apps/prod.env", "cat .npmrc", "cat deploy/credentials.json", "cat .env.staging"]:
        assert violation("shell", {"command": cmd}, c), cmd
    for path in [".env.example", "apps/.env.sample", "src/environment.py"]:
        assert violation("read_file", {"path": path}, c) is None, path


@pytest.mark.skipif(not sandbox.available(), reason="needs macOS sandbox-exec")
def test_os_sandbox_hides_workspace_secrets_from_any_command(ws):
    (ws / ".env").write_text("API_KEY=sk-SECRET\n")
    (ws / ".env.example").write_text("API_KEY=\n")
    (ws / "src" / "prod.env").write_text("TOKEN=sk-SECRET\n")
    cfg = Config(cwd=ws)
    for script in ["grep -r KEY .", "cat .e*", "cat $(printf .en)v", "python3 -c 'print(open(\".env\").read())'", "cat src/prod.env"]:
        r = subprocess.run(sandbox.wrap(["/bin/bash", "-c", script], cfg), cwd=ws, capture_output=True, text=True)
        assert "SECRET" not in r.stdout + r.stderr, script
    r = subprocess.run(sandbox.wrap(["/bin/cat", ".env.example"], cfg), cwd=ws, capture_output=True, text=True)
    assert r.returncode == 0 and "API_KEY=" in r.stdout
