from pydantic import BaseModel, Field

from config.config import Config
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult

OTHER = "Something else (type it)"


class Option(BaseModel):
    label: str = Field(..., description="Short choice, 1-5 words. Add '(Recommended)' to the one you would pick")
    description: str = Field(..., description="One plain sentence on what concretely happens if they pick it, e.g. 'Only users.py changes; billing keeps the old format'")


class Question(BaseModel):
    header: str = Field(..., description="Topic of the decision in 1-3 words, e.g. 'Date format' or 'Which tables'")
    question: str = Field(..., description="One decision, asked in plain words a newcomer to this repo would understand, ending with a question mark")
    context: str = Field(..., description="1-2 sentences: what you found that makes this a question, and exactly what the answer will change (which files, behaviour or part of the task). Name things, never 'it' or 'this'")
    options: list[Option] = Field(..., min_length=2, max_length=4)


class AskUserParams(BaseModel):
    questions: list[Question] = Field(..., min_length=1, max_length=4, description="Up to 4 independent questions asked one after another")


class AskUserTool(Tool):
    name = "ask_user"
    description = (
        "Ask the user to choose when an unknown changes the result and the repo cannot answer it. Use this instead of writing "
        "options and letters in your reply: the user answers with one key. Never use it for permission to run or edit, or for "
        "things you can look up.\n"
        "The user has not seen your tool calls, so every question must stand on its own:\n"
        "- one decision per question, in plain words; explain any internal name, file or term you mention\n"
        "- context says what you found and the exact scope the answer controls (e.g. 'I found 3 callers of parse_date: api.py, "
        "jobs.py, cli.py. This decides which of them switch to UTC.')\n"
        "- 2-4 options that really differ, recommended first, each with a description of its concrete effect; no 'other' option (it is added)\n"
        "How to word it:\n"
        "- talk to the user as 'you', in short everyday sentences, the way a colleague would ask at their desk\n"
        "- ask about the outcome they want, not your internals: 'Should old links keep working?' beats 'Add a redirect shim in router.py?'\n"
        "- name both sides of the choice; no yes/no on a vague verb ('Should I handle this?'), no double negatives, no 'etc.' or 'and so on'\n"
        "- keep it neutral: the recommendation goes on the option, not inside the question\n"
        "- options read as finished answers to the question ('Keep the old format'), not as tasks for you ('Refactor parser')\n"
        "Bad: 'How to handle the legacy config edge case?' / options 'Shim', 'Migrate', 'Ignore'.\n"
        "Good: header 'Old configs', question 'What should happen to config files written before v2?', context 'v2 renamed `port` to "
        "`listen`; 14 example configs in docs/ still use `port`. This decides whether loader.py accepts both names.', options "
        "'Accept both names (Recommended)': old files keep working, loader.py gets a 3-line fallback / 'Only the new name': old files "
        "fail with a clear error telling the user to rename `port`."
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
        questions = AskUserParams(**invocation.params).questions
        answers = []
        for n, q in enumerate(questions, 1):
            step = f" ({n}/{len(questions)})" if len(questions) > 1 else ""
            # first line is the headline, the lines after it are shown dimmed underneath as context
            shown = f"{q.header}{step}: {q.question}\n{q.context}"
            opts = [o.label + f" — {o.description}" for o in q.options] + [OTHER]
            console.bell()
            i = await prompt.ask_choice(console, shown, opts)
            if i == len(q.options):
                text = await prompt.read_line("Your answer (Enter to skip): ")
                answer = text or "(skipped)"
            else:
                answer = q.options[i].label
            console.print(f"  [bold]?[/bold] {q.question} [dim]→[/dim] [bold]{answer}[/bold]", highlight=False)
            answers.append(f"{q.question} -> {answer}")
        return ToolResult.success_result("The user answered:\n" + "\n".join(answers))
