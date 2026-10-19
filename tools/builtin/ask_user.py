from pydantic import BaseModel, Field

from config.config import Config
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult

OTHER = "Something else (type it)"


class Option(BaseModel):
    label: str = Field(..., description="Short choice, 1-5 words. Add '(Recommended)' to the one you would pick")
    description: str = Field("", description="One line on what picking it means or costs")


class Question(BaseModel):
    question: str = Field(..., description="The full question, ending with a question mark")
    options: list[Option] = Field(..., min_length=2, max_length=4)


class AskUserParams(BaseModel):
    questions: list[Question] = Field(..., min_length=1, max_length=4, description="Up to 4 independent questions asked one after another")


class AskUserTool(Tool):
    name = "ask_user"
    description = (
        "Ask the user to choose when an unknown changes the result and the repo cannot answer it. Use this instead of writing "
        "options and letters in your reply: the user answers with one key. 2-4 options per question, recommended first, no "
        "'other' option (it is added). Never use it for permission to run or edit, or for things you can look up."
    )
    kind = ToolKind.READ
    schema = AskUserParams

    def is_mutating(self, params) -> bool:
        return False

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        import sys

        from ui import prompt

        if self.config.model_role != "default" or (prompt.ACTIVE is None and not sys.stdin.isatty()):
            return ToolResult.error_result("No user is available to answer. Pick the most reasonable option yourself and say which one you assumed.")
        from ui.tui import get_console

        console = get_console()
        answers = []
        for q in AskUserParams(**invocation.params).questions:
            opts = [o.label + (f" — {o.description}" if o.description else "") for o in q.options] + [OTHER]
            console.bell()
            i = await prompt.ask_choice(console, q.question, opts)
            if i == len(q.options):
                text = await prompt.read_line("Your answer (Enter to skip): ")
                answer = text or "(skipped)"
            else:
                answer = q.options[i].label
            console.print(f"  [bold]?[/bold] {q.question} [dim]→[/dim] [bold]{answer}[/bold]", highlight=False)
            answers.append(f"{q.question} -> {answer}")
        return ToolResult.success_result("The user answered:\n" + "\n".join(answers))
