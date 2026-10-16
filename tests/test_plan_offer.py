from main import looks_like_plan


def test_plan_detection():
    plan = "Plan to move the pages:\n" + "\n".join(f"{i}. Edit file number {i} so that the import path points at the new folder and still works" for i in range(1, 5))
    assert looks_like_plan(plan)
    assert not looks_like_plan("The handler lives in src/a.ts, line 40.")
    assert not looks_like_plan("- one\n- two\n- three")  # too short to be a plan
