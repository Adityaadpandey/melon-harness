from config.config import Config


async def test_pi_rereads_cost_one_line_or_a_diff(tmp_path):
    from tools.base import ToolInvocation
    from tools.builtin.read_file import ReadFileTool

    f = tmp_path / "a.py"
    f.write_text("\n".join(f"line {i}" for i in range(200)))
    tool = ReadFileTool(Config(cwd=tmp_path))

    async def read(epoch=0, **kw):
        return (await tool.execute(ToolInvocation(params={"path": "a.py", **kw}, cwd=tmp_path, epoch=epoch))).output

    assert "line 199" in await read()
    assert "unchanged" in await read() and "line 100" not in await read()
    f.write_text(f.read_text().replace("line 5\n", "line five\n"))
    diff = await read()
    assert "+line five" in diff and "line 100" not in diff
    assert "line 100" in await read(epoch=1)  # compaction bumped the epoch: the model lost the text
    assert "line 100" in await read(offset=90, limit=20) and "unchanged" not in await read(offset=90, limit=20)

