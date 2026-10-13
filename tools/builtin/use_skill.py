from pydantic import BaseModel, Field

from agent.skills import SECTION_CAP, extract_section, find_in_repo, headings, load_skills
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult


class UseSkillParams(BaseModel):
    name: str = Field(..., description="Skill name from the Skills index")
    section: str | None = Field(
        None,
        description="Optional. 'list' shows the headings of the skill's full reference document; "
        "any other value returns the section whose heading contains that text.",
    )


class UseSkillTool(Tool):
    name = "use_skill"
    description = (
        "Load a project playbook (rules, runbooks, checklists). With no section it returns the "
        "distilled playbook; with `section` it returns that part of the full reference document."
    )
    kind = ToolKind.READ
    schema = UseSkillParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = UseSkillParams(**invocation.params)
        skills = {s.name: s for s in load_skills(self.config)}
        skill = skills.get(p.name)
        if not skill:
            return ToolResult.error_result(f"Unknown skill '{p.name}'. Available: {', '.join(skills) or 'none'}")
        if not p.section:
            return ToolResult.success_result(skill.body)

        if not skill.canonical:
            return ToolResult.error_result(f"Skill '{p.name}' has no reference document; use it without `section`.")
        doc = find_in_repo(invocation.cwd, skill.canonical)
        if not doc:
            return ToolResult.error_result(f"Reference document {skill.canonical} not found from {invocation.cwd}")
        text = doc.read_text(encoding="utf-8", errors="replace")
        if p.section.strip().lower() == "list":
            rows = [f"{'  ' * (lvl - 1)}{title}  (line {ln})" for ln, lvl, title in headings(text) if lvl <= 3]
            return ToolResult.success_result(f"Sections of {skill.canonical}:\n" + "\n".join(rows))
        sec = extract_section(text, p.section)
        if not sec:
            return ToolResult.error_result(f"No section matching '{p.section}'. Use section='list' to see headings.")
        clipped = len(sec) > SECTION_CAP
        return ToolResult.success_result(sec[:SECTION_CAP] + ("\n... [section clipped]" if clipped else ""), truncated=clipped)
