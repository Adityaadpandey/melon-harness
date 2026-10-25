//! Review the way Claude Code's reviews work: split the change, review the parts in parallel, then have a separate agent
//! try to refute every finding before it reaches the user.
//!     working-tree changes -> up to 3 reviewers (one per area, each with its own diff and context)
//!                          -> one verifier per finding (reads the code, may reject it)
//!                          -> a short result block the main agent answers from
//! A reviewer that guesses is cheap to catch here: its HIGH has to survive a second agent that reads the code with no
//! stake in the claim. What the verifier rejects never reaches the answer.
use crate::agent::review::{build_diff, git, review_context};
use crate::agent::review_memory;
use crate::agent::review_repro::{drop_scratch, link_dependencies, make_scratch, parse_repro, MAX_REPROS, REPRO_PROMPT, REPRO_TOOLS};
use crate::agent::review_target::{self, Target};
use crate::agent::verify::{run_check, select_checks, CheckResult};
use crate::config::{ApprovalPolicy, Config};
use crate::util::re::{lit, lit_i, Re};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const MAX_GROUPS: usize = 3;
/// A clipped diff with this many files or fewer stays with one reviewer, who reads the rest with `git diff`.
const SPLIT_FILES: usize = 30;
/// Claims checked per review; past it a finding stays unchecked and is reported as unverified.
const MAX_VERIFIED: usize = 24;
/// Claims one verifier reads at a time, so each gets real turns instead of a share of one budget.
const VERIFY_BATCH: usize = 6;
const SKIP_SUFFIXES: &[&str] = &[".lock", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".woff", ".woff2", ".map", "package-lock.json"];
const GAP_MIN_LINES: usize = 80;
const LIGHT_TURNS: usize = 12; // a small change has little to read; a reviewer that needs more than this is wandering
const SMALL_DIFF_LINES: usize = 80; // one file this small gets one reviewer: a second one re-reads the same few lines
const CHECK_WAIT: u64 = 90; // seconds the reviewers wait for typecheck/lint before starting without it
const TRIAGE_CHUNK: usize = 3; // questions per reviewer: a reviewer holding more than this drifts into skimming all of them
const REGION_PAD: usize = 30; // lines either side of a changed hunk
const REGION_CHARS: usize = 14_000;

fn severity_rank(s: &str) -> usize {
    match s {
        "CRITICAL" => 0,
        "HIGH" => 1,
        "MEDIUM" => 2,
        _ => 3,
    }
}

fn named_scope() -> &'static Re {
    // a named scope is not "the changes"
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(commits?|pr|pull request|HEAD|branch|[0-9a-f]{7,40}|#\d+)\b"))
}
fn security_hint() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(auth\w*|token|password|passwd|secret|permission\w*|tenant\w*|session|cookie|jwt|oauth|csrf|cors|payment\w*|stripe|webhook\w*|role|admin|rbac|acl|sanitiz\w*|escape|exec|eval|innerHTML|queryRaw|\$queryRaw|SELECT|INSERT|DELETE)\b"))
}

// Each reviewer reads the same change through one lens, the way Claude Code runs a bug scan, a rules audit and a history
// check side by side. A single generalist skims; specialists each go deep on one question.
const LENS_BEHAVIOUR: &str = "LENS: behaviour. For every changed function, write down the claim it now makes (\"returns null only when X\"), then read its
callers and callees and try to break the claim. For every new or changed condition, flag or gate, find everything that flips it at
runtime and say what the user sees then (a button disabled while isFetching, and every mutation refetches it: it dims on every tap).
Compare the new version of each component, page or function with the old one (`git show HEAD:<file>` or the base revision): list what the
old one offered (actions, states, copy, guards, escape hatches) and say what the user can no longer do. A one-time secret or irreversible
result that a stray click, Escape or backdrop tap can dismiss is a defect. Check wrong operands, inverted conditions, stale state, async ordering, races, leaks, off-by-one, null paths. Read what the diff deleted
before saying a replacement is missing: the new code often leans on a library doing that work.
Removed behaviour: for every line the diff deletes or replaces, name the invariant it enforced (a guard, an error path, a validation, a
cleanup, an early exit), then find where the new code establishes it again. If you cannot find it, that is a finding. Also ask what a
line that moved into a new branch does now when the branch is not taken.";
const LENS_APPROACH: &str = "LENS: approach. Judge the change the way the owner of this system would, not line by line. Is this the right fix at the right layer,
or a patch on the symptom while the cause stays (a guard in one caller when every caller shares the broken function)? Is there a simpler
change that does the same job (delete the branch, reuse the helper that already exists a few files over, change the data instead of the code)?
Layering: does it use the layer the repo's rules and neighbouring files use (service -> repository -> database), or reach past it?
Duplication the diff creates: the same rule written in two or three places (cache hit, cache miss, public endpoint) belongs in one helper.
Mirrors: when the change touches one of a pair (desktop and mobile layout, create and update, cached and uncached path, server and
client), open the other half and say whether it needs the same change. What is the blast radius: who else calls or deploys this, does old data, an old client or a half-finished rollout still work, can it be
rolled back? Does it follow how the neighbouring code solves the same problem, and if it does not, is the new way better? A real concern
about the design is a finding even when every line is correct: name the better shape and what it saves. Production readiness: a schema or
data-shape change needs a migration or a backfill; a changed API, event or config key must keep old callers and stored data working.";
const LENS_FAILURES: &str = "LENS: failures and contracts. For every new awaited call or write on a request path (DB insert, cache write, outbound fetch), ask
what the user gets when it throws; optional data that now fails the whole response is a finding. Look for swallowed errors, empty
catches and fallbacks that hide a real failure. For every changed signature, return shape, event, route, schema, env var, cache key or
migration, grep every caller and consumer and open the line that reads it: the type may say one thing and the runtime data another.
Missing tests: code that guards privacy, auth, money or a permission needs a test, so none in the diff is a finding (the repo's
rules or playbooks usually say so: quote them; check whether a spec file exists on the branch but is missing from the diff).
Performance: a new query, await or loop on a per-request path is a finding when it repeats per item or per visit (an extra DB call
on every cache hit, an N+1); say how often it runs. Accessibility: UI hidden, locked or disabled by CSS alone (pointer-events: none,
opacity) stays focusable and is still read aloud; it needs the attribute or the element removed.";
const LENS_RULES: &str = "LENS: rules and intent. Check the diff against the PROJECT RULES and PLAYBOOKS above: every finding quotes the rule in a few
words and the line that breaks it; a rule that is about how the author works (never commit, ask first) is not a review finding. Then
check the request. A request is a vision, not a full spec: for behaviour it is silent on, a reasonable user's expectation is a requirement
(a delete button that works on every row but the last is a defect even though nobody wrote \"the last row\"). Is something it asked for missing from the diff, and is something it did not ask for in it (scope creep, a new
dependency, a rewrite of working code)? Also read the comments in the touched files: does the change break guidance written there?";
const LENS_HISTORY: &str = "LENS: history. Use git: `git log -5 --oneline -- <file>`, `git blame` or `git log -L` on the changed region, `git show HEAD:<file>`.
Look for a change that undoes an earlier fix (read that commit's message), an invariant a previous commit set up that the diff
breaks, and a sibling the change should have followed (grep for the same pattern next door: if five handlers do X and the new one
does not, say why that is a defect). A defect the old code already had is not this change's.";
const LENSES: &[(&str, &str)] = &[("behaviour", LENS_BEHAVIOUR), ("approach", LENS_APPROACH), ("failures", LENS_FAILURES), ("rules", LENS_RULES), ("history", LENS_HISTORY)];
const CONTRACT_LENS: (&str, &str) = ("contracts", "LENS: producers and consumers. Nothing else is your job. List every value, shape, string format, prop, event, route,
cache key or return type the diff creates or changes. For each, find BOTH sides: the code that produces it and the code that reads it
(grep the name, open every hit). Then compare them line against line, with a real example value pushed through each side:
- A consumer that parses, matches or normalises something the producer already shapes (a regex that wants \"3 - Name\" when the
  producer writes \"3x Name\"; a trim or a null check the other side already did) never fires: that is dead code, name both lines.
- A producer that now emits a new shape, null, empty list, enum member or error that a consumer, type, test or API collection still
  assumes away; a field renamed on one side only; a unit, timezone or casing change.
- Both halves of a pair (desktop/mobile, create/update, server/client, cached/uncached): one changed and the twin did not.
- A new fetch, query or lookup whose answer the caller already holds in a payload it received: say which payload and which field.
Quote the producer line and the consumer line in the finding (path:line each) and the example value that breaks it. Report only
mismatches you proved by reading both sides.");
const SECURITY_LENS: (&str, &str) = ("security", "LENS: security and isolation. The diff touches auth, data access, secrets or money, so trace who can reach it: is every new
route, handler or query behind the same authorization its siblings use? Is a tenant, owner or user id taken from the request instead
of the session? Injection (SQL, shell, path, HTML) from any value that crosses a trust boundary, secrets or tokens in logs and
responses, fail-open checks (an error path that returns \"allowed\"), missing rate limits or idempotency on a write that moves money
or sends mail. Read the middleware and the sibling handlers before claiming a gap.");
const GAP_LENS: &str = "LENS: what the others missed. Four reviewers, each with one angle, already reported the findings below. Do not repeat
them. Re-read the riskiest hunk of the diff yourself and ask the question none of the angles owns: what input, ordering or state
did nobody try? If one of their findings is wrong, say so on a line starting with REJECT.
ALREADY REPORTED:
{found}";
const LIGHT_NOTE: &str = "

QUICK PASS: this change is small and the whole diff is above. Do not explore. Read the changed region's file once if you need the lines
around it (and the script header or config it runs under), at most 3 reads in all, then answer. Cover: a failure the new code can cause
(a command that can exit non-zero under the shell options in force, an await that can throw), anything it deletes or widens (a flag that
removes more than before), a second copy of something the file already does, and a project rule the edit breaks (quote it).
Where you did not check something, say so in the finding (\"unverified: the script header was not read\").";

const REVIEWER_PROMPT: &str = "You are a principal engineer reviewing {scope}. You did not write it, and you will be accountable if it ships broken.
The request: {request}

FILES: {part}
{moved}OTHER CHANGED FILES (read them only to follow a call chain): {others}

{context}DIFF:
{diff}

{lens}

Hold the bar a principal engineer holds: find the defect that pages someone at 3am, not the ten that make a review look busy. Explain each finding
with the trigger (\"when X happens\"), the path that reaches it, and what the user or the system does then. Prefer the root cause over the symptom.

How to work: first list, for yourself, every behaviour the diff changes and what could go wrong with each; then check each one by reading
with your tools (callers, callees, config, tests, `git show HEAD:<file>`). Read before you claim; a claim about how a library behaves counts only
if you read the library or its docs. Say what the fix should be in the smallest form: deleting code, bumping a cache key or version, or
removing a branch beats adding a try/catch or a fallback. One line per defect, under 45 words, in plain words (no section numbers, no quoting whole paragraphs): the same cause hit in two places is one finding naming both.
Report what the change itself introduces even when it compiles:
- a failure it can cause: a command that can exit non-zero under the shell options in force (`set -e`, `pipefail`), an await or write that can throw on a path that used to succeed, and what the caller or the next job then does with it (a deploy marked failed after the app was already healthy);
- something it removes or widens: a guard, error path, validation or early exit the old code had, or an option that makes a command touch more than before (`prune -a`, `rm -r`, `--force`, a wider glob or query) and what then goes missing;
- a feature, action or state the old code offered that the new one silently loses (a back button, an empty state, a way to recover a one-time value);
- a branch or route nothing reaches; the same work now done twice (a second copy of a step the file or pipeline already runs); one thing named two ways across the surfaces that show it;
- a visible restyle or an edit the project rules forbid or say to confirm first: quote the rule and the line that breaks it; a doc that now disagrees with the code.
Never report: style or formatting that changes nothing (indentation counts only where it changes meaning, as in YAML or Python), redundancy that was already there,
anything a linter or typechecker catches, defects on lines the diff did not touch, defects the old code already had, behaviour that is clearly the intent of the change,
pedantic nitpicks a senior engineer would not raise, missing tests or docs unless the project rules ask for them, a rule the code explicitly silences (a lint-ignore comment),
or anything you cleared in your own thinking. A finding you could not fully check is still worth one line: say what you did not check (\"unverified: the script header was not read\").

Reply with at most 10 lines, one finding per line, EXACTLY:
SEVERITY | path:line | what is wrong, and who is affected (the call chain you read) | fix
SEVERITY is HIGH only for a defect you proved by reading the code; a guess is MEDIUM and says \"unverified\".
If you found nothing, reply: NONE
Before the findings put these lines (each on its own line):
SUMMARY: <one or two sentences saying what the change does, in plain words>
STRENGTH: <one specific thing the change does well, with file:line; leave the line out if nothing stands out, never flatter>
ASIDE: <a behaviour you looked at and set aside as out of scope or intended, and why; at most 2 such lines, none if none>";

const VERIFIER_PROMPT: &str = "Another agent claims these defects exist in the working tree. Decide each one. Your default is PLAUSIBLE: refuting a real defect costs more than keeping a doubtful one a reader can discard.

CLAIMS:
{claims}

For each claim read the cited code, then the call chain that reaches it (callers, the route or handler, config that changes behaviour), and
check whether something already handles it or the behaviour is intended (read the lines the diff deleted too: `git diff` shows them).
A claim about how a library behaves counts only if you read the library or its docs. If a claim names a consumer (a component,
controller or caller that reads the data), open that file and find the exact line that reads it; if that line does not read what the
claim says, REJECT it: a wrong consumer is the usual way a plausible claim is false.

Also settle how bad it is. Is the failing state permanent, or does it go away by itself (entries that expire, a one-time migration,
a window after deploy)? Did the code at HEAD (`git show HEAD:<file>`) already fail the same way, so the change did not add it?
Say ALREADY_AT_HEAD too when the request already depends on the same thing and fails without the change whenever this fails
(a new write to the same Postgres the request already reads: if Postgres is down it 500s anyway; only \"reads work, the write fails\" is new).
A claim that cites a project rule counts only if that rule exists in the instruction file and the cited line really breaks it
(open the file: a rule paraphrased from memory is REJECTED). A claim about code the diff did not touch is REJECTED. So is a pedantic nitpick a senior engineer would not raise, a
general quality complaint (missing tests, docs, \"poor design\") the project rules do not ask for, and a rule the code explicitly silences.
If two claims are the same cause (one missing guard hit from several callers), write SAME_AS <the lower number> on the later one.

Think each claim through before you answer: what input or state triggers it, who gets there, and what they see. Reply with one line per claim, EXACTLY:
N | CONFIRMED or PLAUSIBLE or REJECTED or UNCLEAR | path:line what you read that decides it | who is actually affected (n/a if rejected) | ONGOING or TEMPORARY (in under 10 words, e.g. \"cache entries before deploy, 15 min\") | NEW or ALREADY_AT_HEAD | NONE or SAME_AS <number> | CONF <0-100>
CONF is how sure you are the defect is real AND will be hit in practice: 0 a false positive or pre-existing, 25 maybe, 50 real but rare or minor,
75 verified and will be hit, 100 certain and frequent. A function whose documented contract the change breaks is 75 or more even when this repo
has no other caller: callers are outside the diff, and the next one gets the wrong answer. Judge it as a senior engineer would.
CONFIRMED means you can name the input or state that triggers it and the wrong result, and you read the code that proves it.
PLAUSIBLE means the mechanism is real but the trigger is uncertain (timing, environment, config, a rare but reachable path); say what would confirm it.
Do not reject a claim for being \"speculative\" or \"depends on runtime state\" when that state is realistic: races, a failing command under `set -e`,
null on an error path, a cold cache, a boundary the code does not exclude, a partial failure are PLAUSIBLE.
REJECTED only when you can show it: the code contradicts the claim (quote the line), the case is provably impossible (type, constant, invariant),
the diff already guards it (cite the guard), or it is pure style with no observable effect. Say where.
UNCLEAR means you could not decide with the files available. Nothing else.";

const TRIAGE_PROMPT: &str = "You are planning a code review, not doing it. Read the change below and decide what is worth checking, so the reviewers
spend their effort there instead of re-reading everything.

{context}DIFF ({files}):
{diff}

List the specific things in THIS change that could break something: a changed condition, a deleted or moved guard, error path or exit whose job the new code may not do any more, a new awaited call or
write, a changed signature or shape someone else consumes, a command that now runs somewhere it did not, a rule from the project notes it may break.
You may grep or read a file to see who consumes something, but do not review. Skip what only moves text around (comments, copy, formatting).
Reply with at most 6 lines, EXACTLY:
CHECK | path:line | the question a reviewer must answer by reading code (name the consumer or caller to read when you know it)
If nothing in the change can plausibly break anything, reply: NONE";

/// All the angles as one checklist for ONE reviewer. Claude Code's own /code-review gets nine findings out of a 22-file PR
/// from a single agent in under a minute; what matters is the method and the budget, not how many agents there are.
fn one_reviewer(context: &str, security: bool) -> (String, String) {
    let mut parts: Vec<&str> = vec![LENSES[0].1, LENSES[1].1, LENSES[2].1, if context.contains("PROJECT RULES") { LENSES[3].1 } else { LENSES[4].1 }];
    if security {
        parts.push(SECURITY_LENS.1);
    }
    ("review".into(), format!("Work through every angle below in turn, spending most of your effort where the diff is riskiest:\n\n{}", parts.join("\n\n")))
}

pub fn parse_triage(reply: &str) -> Vec<String> {
    let re = lit_i(r"(?m)^\W*CHECK\s*\|\s*([^|\n]+?)\s*\|\s*(.+)$");
    let (mut out, mut at) = (vec![], 0);
    while at < reply.len() {
        let Some(c) = re.captures(&reply[at..]) else { break };
        let (_, e) = re.find(&reply[at..]).unwrap();
        out.push(format!("{}: {}", c[1].clone().unwrap_or_default().trim(), c[2].clone().unwrap_or_default().trim()));
        at += e.max(1);
    }
    out.truncate(6);
    out
}

/// The questions the triage picked, dealt out TRIAGE_CHUNK at a time. Three questions are one reviewer; six are two.
fn triage_lenses(checks: &[String]) -> Vec<(String, String)> {
    checks
        .chunks(TRIAGE_CHUNK)
        .enumerate()
        .map(|(i, chunk)| {
            let rows = chunk.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n");
            (if i == 0 { "review" } else { "focus" }.to_string(), format!("WHAT TO CHECK (a triage read this diff and picked these; answer each by reading the code, then report anything else this diff breaks that they missed):\n{rows}"))
        })
        .collect()
}

async fn triage(config: &Config, role: &str, diff: &str, files: &[String], context: &str) -> Vec<String> {
    let prompt = TRIAGE_PROMPT.replace("{context}", context).replace("{files}", &files.iter().take(25).cloned().collect::<Vec<_>>().join(", ")).replace("{diff}", diff);
    let reply = ask(config, &prompt, role, 4, config.review_pipeline_tokens.min(25_000), Some(vec!["code-review".into()]), None, "never").await;
    parse_triage(&reply)
}

/// The current text around every changed hunk, with line numbers, so a quick reviewer sees the shell options, the loop or
/// the function a hunk sits in without spending reads on it.
fn changed_regions(cwd: &Path, diff: &str) -> String {
    let hunk = lit(r"(?m)^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@");
    let header = lit(r"^a/(.+?) b/(.+)");
    let (mut out, mut used) = (vec![], 0usize);
    for block in diff.split("\ndiff --git ").skip(1) {
        let Some(first) = block.lines().next() else { continue };
        let Some(c) = header.captures(first) else { continue };
        let path = c[2].clone().unwrap_or_default();
        let Ok(text) = std::fs::read(cwd.join(&path)).map(|b| String::from_utf8_lossy(&b).to_string()) else { continue };
        let lines: Vec<&str> = crate::util::search::split_lines(&text);
        let mut spans: Vec<(usize, usize)> = vec![];
        let mut at = 0;
        while let Some(h) = hunk.captures(&block[at..]) {
            let (_, e) = hunk.find(&block[at..]).unwrap();
            let start: usize = h[1].as_ref().and_then(|x| x.parse().ok()).unwrap_or(1);
            let count: usize = h.get(2).cloned().flatten().and_then(|x| x.parse().ok()).unwrap_or(1);
            let (a, b) = (start.saturating_sub(REGION_PAD).max(1), (start + count.max(1) - 1 + REGION_PAD).min(lines.len()));
            match spans.last_mut() {
                Some(l) if a <= l.1 + 1 => l.1 = l.1.max(b),
                _ => spans.push((a, b)),
            }
            at += e.max(1);
        }
        for (a, b) in spans {
            if a > b {
                continue;
            }
            let t = format!("--- {path} lines {a}-{b} (current version)\n{}", (a..=b).map(|n| format!("{n}| {}", lines[n - 1])).collect::<Vec<_>>().join("\n"));
            if used + t.len() > REGION_CHARS {
                return format!("{}\n[more regions cut for size: read the file]", out.join("\n"));
            }
            used += t.len();
            out.push(t);
        }
    }
    out.join("\n")
}

/// A change this small needs no plan: up to two files, under SMALL_DIFF_LINES of diff, nothing touching auth, queries or money.
fn is_light(diff: &str, files: &[String], secure: bool) -> bool {
    files.len() <= 2 && diff.matches('\n').count() < SMALL_DIFF_LINES && !secure
}

/// Auto (review_lenses = 0) is ONE reviewer carrying every angle plus one that only follows producers to consumers.
/// review_lenses = N >= 2 fans out to N separate angle reviewers (security is appended when auth, queries or money are
/// touched); 1 pins the behaviour angle alone.
fn pick_lenses(config: &Config, diff: &str, files: &[String], context: &str) -> Vec<(String, String)> {
    let secure = security_hint().is_match(&format!("{}\n{diff}", files.join(" ")));
    if config.review_lenses == 0 {
        if files.len() == 1 && diff.matches('\n').count() < SMALL_DIFF_LINES {
            return vec![one_reviewer(context, secure)];
        }
        return vec![one_reviewer(context, secure), (CONTRACT_LENS.0.into(), CONTRACT_LENS.1.into())];
    }
    let mut picked: Vec<(String, String)> = LENSES.iter().take(config.review_lenses).map(|(n, t)| (n.to_string(), t.to_string())).collect();
    if config.review_lenses != 1 && secure {
        picked.push((SECURITY_LENS.0.into(), SECURITY_LENS.1.into()));
    }
    picked
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub severity: String,
    pub where_: String,
    pub what: String,
    pub fix: String,
    pub verdict: String,
    pub evidence: String,
    pub reach: String,
    /// How long a self-healing failing state lasts, empty when it stays.
    pub temporary: String,
    /// The code before the change already failed the same way.
    pub at_head: bool,
    /// Claim number this one repeats (one cause, several callers); 0 when it stands alone.
    pub same_as: usize,
    /// What running the code showed: "ran X: <output line>", or that it could not be made to happen.
    pub repro: String,
    /// The verifier's 0-100 belief that this is real and will be hit; below the cutoff it is dropped.
    pub confidence: i64,
}

impl Default for Finding {
    fn default() -> Self {
        Finding { severity: String::new(), where_: String::new(), what: String::new(), fix: String::new(), verdict: "UNCHECKED".into(), evidence: String::new(), reach: String::new(), temporary: String::new(), at_head: false, same_as: 0, repro: String::new(), confidence: 100 }
    }
}

fn changed_files(porcelain: &str) -> Vec<String> {
    let mut files = vec![];
    for line in porcelain.lines() {
        if line.len() < 4 || line[..2].contains('D') {
            continue;
        }
        let path = line[3..].rsplit(" -> ").next().unwrap_or("").trim().trim_matches('"').to_string();
        if !path.is_empty() && !SKIP_SUFFIXES.iter().any(|s| path.ends_with(s)) {
            files.push(path);
        }
    }
    files
}

fn deleted_files(porcelain: &str) -> Vec<String> {
    porcelain.lines().filter(|l| l.len() >= 4 && l[..2].contains('D')).map(|l| l[3..].trim().trim_matches('"').to_string()).collect()
}

/// A file deleted in one place and added somewhere else with identical bytes is a move, not a change.
/// Returns (moves, deleted left over, added left over).
async fn pair_moves(cwd: &Path, deleted: &[String], added: &[String]) -> (Vec<(String, String)>, Vec<String>, Vec<String>) {
    let mut gone: HashMap<String, Vec<String>> = HashMap::new();
    for d in deleted {
        let sha = git(cwd, &["rev-parse", &format!("HEAD:{d}")]).await.trim().to_string();
        if sha.len() == 40 {
            gone.entry(sha).or_default().push(d.clone());
        }
    }
    let (mut moves, mut used_old, mut moved_new) = (vec![], vec![], vec![]);
    for a in added {
        if !cwd.join(a).is_file() {
            continue;
        }
        let sha = git(cwd, &["hash-object", "--", a]).await.trim().to_string();
        if let Some(list) = gone.get_mut(&sha) {
            if !list.is_empty() {
                let old = list.remove(0);
                used_old.push(old.clone());
                moved_new.push(a.clone());
                moves.push((old, a.clone()));
            }
        }
    }
    (moves, deleted.iter().filter(|d| !used_old.contains(d)).cloned().collect(), added.iter().filter(|a| !moved_new.contains(a)).cloned().collect())
}

/// Files still sitting where the moved ones used to be. They may have been leaning on what moved.
async fn left_behind(cwd: &Path, moves: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (old, _) in moves {
        let folder = old.rsplit_once('/').map_or(".", |x| x.0);
        for f in git(cwd, &["ls-files", "--", folder]).await.split('\n') {
            if !f.is_empty() && !out.iter().any(|o| o == f) && !moves.iter().any(|(o, _)| o == f) && cwd.join(f).exists() {
                out.push(f.to_string());
            }
        }
    }
    out.truncate(15);
    out
}

/// One readable sentence or two: model debris after a stray tag is cut, long text is clipped at a word.
pub fn tidy(text: &str, limit: usize) -> String {
    let cut = text.split("<tool_call").next().unwrap_or("").split("]<").next().unwrap_or("").split("[<").next().unwrap_or("").trim().trim_end_matches(|c| ".;, ".contains(c)).to_string();
    if cut.chars().count() <= limit {
        return cut;
    }
    let head: String = cut.chars().take(limit).collect();
    format!("{}...", head.rsplit_once(' ').map_or(head.as_str(), |x| x.0))
}

/// What a human reviewer writes around the findings: what the change does, what was and was not looked at.
fn frame(answer: &str, summary: &str, reviewed: &[String], moves: &[(String, String)], deleted: &[String], checks: &[CheckResult], strengths: &[String], scope: &str) -> String {
    let mut lines: Vec<String> = vec![];
    if !summary.is_empty() {
        lines.push(format!("**What the change does:** {}.", tidy(summary, 320)));
    }
    if let Some(g) = strengths.iter().map(|x| tidy(x, 200)).find(|x| !x.is_empty()) {
        lines.push(format!("**What is good:** {g}."));
    }
    lines.push(answer.to_string());
    let mut seen = vec![format!("{} file(s)", reviewed.len())];
    if !moves.is_empty() {
        seen.push(format!("{} moved unchanged ({}{})", moves.len(), moves.iter().take(3).map(|(a, b)| format!("{a} -> {b}")).collect::<Vec<_>>().join(", "), if moves.len() > 3 { ", ..." } else { "" }));
    }
    if !deleted.is_empty() {
        seen.push(format!("{} deleted", deleted.len()));
    }
    let bad: Vec<&str> = checks.iter().filter(|c| !c.ok).map(|c| c.command.as_str()).collect();
    let ran = if checks.is_empty() {
        "I did not run typecheck, lint or tests.".to_string()
    } else if !bad.is_empty() {
        format!("Checks that FAILED: {}.", bad.join(", "))
    } else {
        format!("{} project check(s) passed (typecheck, lint, tests).", checks.len())
    };
    lines.push(format!("_{}I read {}. {}{}_", if scope.is_empty() { String::new() } else { format!("Scope: {scope} ") }, seen.join(", "), ran, if moves.is_empty() { "" } else { " Stage the move with git add -A on both paths so git keeps the rename." }));
    lines.join("\n\n")
}

/// A diff too big for one prompt is cut by area; a change that lives in one area is cut into thirds.
fn split_clipped(files: &[String]) -> Vec<(String, Vec<String>)> {
    let groups = group_files(files, MAX_GROUPS);
    if groups.len() == 1 && files.len() > 1 {
        return (0..MAX_GROUPS.min(files.len())).map(|i| (format!("part {}", i + 1), files.iter().skip(i).step_by(MAX_GROUPS).cloned().collect())).collect();
    }
    groups
}

/// Group by area (apps/server, apps/web, ...). More areas than reviewers: the extra ones are folded into the smallest group.
fn group_files(files: &[String], limit: usize) -> Vec<(String, Vec<String>)> {
    let mut areas: Vec<(String, Vec<String>)> = vec![];
    for f in files {
        let parts: Vec<&str> = f.split('/').collect();
        let key = if parts.len() > 2 { parts[..2].join("/") } else { parts[0].to_string() };
        match areas.iter_mut().find(|a| a.0 == key) {
            Some(a) => a.1.push(f.clone()),
            None => areas.push((key, vec![f.clone()])),
        }
    }
    areas.sort_by(|a, b| b.1.len().cmp(&a.1.len())); // stable: ties keep first-seen order
    while areas.len() > limit {
        let (name, fs) = areas.pop().unwrap();
        let small = areas.last_mut().unwrap();
        small.0 = format!("{} + {name}", small.0);
        small.1.extend(fs);
    }
    areas
}

pub fn parse_findings(text: &str) -> Vec<Finding> {
    let mut out = vec![];
    for line in text.lines() {
        let l = line.trim().trim_start_matches(|c: char| "-*•0123456789. ".contains(c));
        let parts: Vec<String> = l.split('|').map(|p| p.trim().to_string()).collect();
        if parts.len() >= 3 && matches!(parts[0].to_uppercase().as_str(), "CRITICAL" | "HIGH" | "MEDIUM" | "LOW") {
            out.push(Finding { severity: parts[0].to_uppercase(), where_: parts[1].clone(), what: parts[2].clone(), fix: parts.get(3).cloned().unwrap_or_default(), ..Default::default() });
        }
    }
    out
}

/// (verdict, evidence, reach, temporary, at_head, same_as, confidence)
type Verdict = (String, String, String, String, bool, usize, i64);

/// One `N | VERDICT | evidence | reach | ONGOING/TEMPORARY (how long) | NEW/ALREADY_AT_HEAD | SAME_AS | CONF n` line per
/// claim. A claim the verifier skipped stays UNCLEAR; the trailing fields are optional.
fn parse_verdicts(text: &str, count: usize) -> Vec<Verdict> {
    let mut out: Vec<Verdict> = (0..count).map(|_| ("UNCLEAR".into(), String::new(), String::new(), String::new(), false, 0, 100)).collect();
    let line = lit_i(r"(?m)^\W*(\d+)\s*\|\s*(CONFIRMED|PLAUSIBLE|REJECTED|UNCLEAR)\s*\|(.*)$");
    let twin_re = lit_i(r"SAME_AS\s*#?(\d+)");
    let conf_re = lit(r"(\d{1,3})");
    let mut at = 0;
    while at < text.len() {
        let Some(m) = line.captures(&text[at..]) else { break };
        let (_, e) = line.find(&text[at..]).unwrap();
        at += e.max(1);
        let n: usize = m[1].as_ref().and_then(|x| x.parse().ok()).unwrap_or(0);
        if n == 0 || n > count {
            continue;
        }
        let mut rest: Vec<String> = m[3].clone().unwrap_or_default().split('|').map(|p| p.trim().to_string()).collect();
        while rest.len() < 6 {
            rest.push(String::new());
        }
        let twin = twin_re.captures(&rest[4]).and_then(|c| c[1].as_ref().and_then(|x| x.parse().ok())).unwrap_or(0);
        let conf = conf_re.captures(&rest[5]).and_then(|c| c[1].as_ref().and_then(|x| x.parse::<i64>().ok())).map(|c| c.min(100)).unwrap_or(100);
        let lasts = if rest[2].to_uppercase().starts_with("TEMPORARY") {
            let l = lit_i(r"^TEMPORARY\b[\s:(-]*").replace(&rest[2], "").trim_end_matches(|c| ") ".contains(c)).to_string();
            if l.is_empty() { "temporary".to_string() } else { l }
        } else {
            String::new()
        };
        out[n - 1] = (m[2].clone().unwrap_or_default().to_uppercase(), rest[0].chars().take(400).collect(), rest[1].chars().take(200).collect(), lasts.chars().take(120).collect(), rest[3].to_uppercase().contains("ALREADY"), twin, conf);
    }
    out
}

/// What the user sees. HIGH has to be proven AND lasting AND new: a claim nobody proved, a state that heals itself or one
/// the old code already had is at most MEDIUM.
fn shown_severity(f: &Finding) -> &str {
    if matches!(f.severity.as_str(), "CRITICAL" | "HIGH") && (f.verdict != "CONFIRMED" || !f.temporary.is_empty() || f.at_head) {
        return "MEDIUM";
    }
    &f.severity
}

/// The verifier saw every claim at once: a claim it marked `SAME_AS 1` is folded into claim 1 (even across files).
fn merge_same_cause(mut checked: Vec<Finding>) -> Vec<Finding> {
    let mut keep: Vec<usize> = vec![];
    for n in 1..=checked.len() {
        let f_same = checked[n - 1].same_as;
        let twin_idx = if f_same > 0 && f_same < n { Some(f_same - 1) } else { None };
        match twin_idx {
            Some(t) if checked[t].verdict != "REJECTED" && checked[n - 1].verdict != "REJECTED" => {
                let (where_, at_head, temporary) = (checked[n - 1].where_.clone(), checked[n - 1].at_head, !checked[n - 1].temporary.is_empty());
                let twin = &mut checked[t];
                twin.where_.push_str(&format!(", {where_}"));
                twin.at_head = twin.at_head && at_head;
                if !temporary {
                    twin.temporary.clear(); // one lasting place makes the whole thing lasting
                }
            }
            _ => keep.push(n - 1),
        }
    }
    keep.into_iter().map(|i| checked[i].clone()).collect()
}

/// The same cause reported twice (one fix, same file) becomes one finding naming every place.
fn merge_duplicates(findings: Vec<Finding>) -> Vec<Finding> {
    let words = |t: &str| -> std::collections::HashSet<String> {
        let re = lit(r"[a-z]{4,}");
        let (mut s, mut at, low) = (std::collections::HashSet::new(), 0, t.to_lowercase());
        while let Some((a, b)) = re.find(&low[at..]) {
            s.insert(low[at + a..at + b].to_string());
            at += b.max(1);
        }
        s
    };
    let alike = |a: &str, b: &str| {
        let (wa, wb) = (words(a), words(b));
        !wa.is_empty() && !wb.is_empty() && wa.intersection(&wb).count() as f64 >= 0.6 * wa.len().min(wb.len()) as f64
    };
    let mut merged: Vec<Finding> = vec![];
    for f in findings {
        let file = f.where_.split(':').next().unwrap_or("").to_string();
        let same = merged.iter().position(|m| m.where_.split(':').next().unwrap_or("") == file && ((!m.fix.is_empty() && !f.fix.is_empty() && alike(&m.fix, &f.fix)) || alike(&m.what, &f.what)));
        match same {
            None => merged.push(f),
            Some(i) => {
                let place = f.where_.split_once(':').map_or(f.where_.clone(), |x| x.1.to_string());
                if !merged[i].where_.contains(&place) {
                    merged[i].where_.push_str(&format!(", {place}")); // four angles hitting the same line name it once
                }
            }
        }
    }
    merged
}

/// The block the main agent answers from. Rejected findings are listed so it can say what was ruled out, but never as findings.
fn compose(findings: &[Finding], reviewers: usize) -> String {
    let mut live: Vec<&Finding> = findings.iter().filter(|f| matches!(f.verdict.as_str(), "CONFIRMED" | "PLAUSIBLE" | "UNCLEAR" | "UNCHECKED")).collect();
    live.sort_by_key(|f| severity_rank(shown_severity(f)));
    let rejected: Vec<&Finding> = findings.iter().filter(|f| f.verdict == "REJECTED").collect();
    let mut lines = vec![format!("{reviewers} reviewer(s) read the change in parallel, each through its own angle; each finding was then checked by a separate agent that tried to refute it.")];
    if live.is_empty() && rejected.is_empty() {
        lines.push("No reviewer found a defect.".into());
    }
    for f in live {
        let tag = if f.verdict == "CONFIRMED" { "confirmed" } else { "unverified" };
        lines.push(format!(
            "- [{tag}] {} {}  {} -> {}{}{}{}",
            shown_severity(f),
            f.where_,
            f.what,
            f.fix,
            if f.evidence.is_empty() { String::new() } else { format!("  (evidence: {})", f.evidence) },
            if !f.reach.is_empty() && f.reach != "n/a" { format!("  (affects: {})", f.reach) } else { String::new() },
            if f.repro.is_empty() { String::new() } else { format!("  (repro: {})", f.repro) }
        ));
    }
    for f in rejected {
        lines.push(format!("- [rejected] {}  {}  because: {}", f.where_, f.what.chars().take(90).collect::<String>(), if f.evidence.is_empty() { "the code contradicts it" } else { &f.evidence }));
    }
    lines.join("\n")
}

/// A finding that cites a file that is not in the repo (and not one the change deleted) is a reviewer's slip.
fn drop_missing_files(findings: Vec<Finding>, files: &[String]) -> Vec<Finding> {
    findings.into_iter().filter(|f| !f.where_.contains("(file not found)") || files.contains(&f.where_.split(':').next().unwrap_or("").trim().replace(" (file not found)", ""))).collect()
}

/// `position` maps a finding to its number in the whole list: SAME_AS in a reply counts inside the batch it saw.
fn apply_verdicts(findings: &mut [Finding], range: std::ops::Range<usize>, reply: &str, min_conf: i64) {
    let verdicts = parse_verdicts(reply, range.len());
    for (k, v) in verdicts.into_iter().enumerate() {
        let (verdict, evidence, reach, temporary, at_head, same_as, conf) = v;
        let same_global = if same_as > 0 { if same_as <= range.len() { range.start + same_as } else { 0 } } else { 0 };
        let f = &mut findings[range.start + k];
        f.verdict = verdict;
        f.evidence = evidence;
        f.reach = reach;
        f.temporary = temporary;
        f.at_head = at_head;
        f.same_as = same_global;
        f.confidence = conf;
        if f.verdict != "REJECTED" && f.at_head {
            // the old code failed the same way: not this change's defect
            f.verdict = "REJECTED".into();
            f.evidence = "already present before this change".into();
        }
        if f.verdict != "REJECTED" && f.verdict != "PLAUSIBLE" && f.confidence < min_conf {
            // a plausible claim is low confidence by definition
            f.verdict = "REJECTED".into();
            f.evidence = format!("low confidence ({}/100): {}", f.confidence, f.evidence);
        }
    }
}

fn claim_lines(batch: &[Finding]) -> String {
    batch.iter().enumerate().map(|(n, f)| format!("{}. {} | {} | {} | fix: {}", n + 1, f.severity, f.where_, f.what, if f.fix.is_empty() { "(none)" } else { &f.fix })).collect::<Vec<_>>().join("\n")
}

/// Claims go to verifiers six at a time (in parallel), so each claim gets turns of its own. A claim the verifier could not
/// decide gets one more look from a fresh verifier with a bigger budget; if it is still undecided it stays PLAUSIBLE.
async fn verify(config: &Config, role: &str, findings: &mut Vec<Finding>) {
    let ranges: Vec<std::ops::Range<usize>> = (0..findings.len()).step_by(VERIFY_BATCH).map(|i| i..(i + VERIFY_BATCH).min(findings.len())).collect();
    let replies = {
        let futs = ranges.iter().map(|r| {
            let batch = &findings[r.clone()];
            let prompt = VERIFIER_PROMPT.replace("{claims}", &claim_lines(batch));
            let turns = 30.min(10 + 3 * batch.len());
            async move { ask(config, &prompt, role, turns, config.review_pipeline_tokens, Some(vec!["code-review".into()]), None, "never").await }
        });
        futures::future::join_all(futs).await
    };
    for (r, reply) in ranges.into_iter().zip(replies) {
        apply_verdicts(findings, r, &reply, config.review_min_confidence);
    }
    let unclear: Vec<usize> = (0..findings.len()).filter(|i| findings[*i].verdict == "UNCLEAR").collect();
    if !unclear.is_empty() {
        let batch: Vec<Finding> = unclear.iter().map(|i| findings[*i].clone()).collect();
        let prompt = format!("A first verifier could not decide these. Settle each: open the files it would have needed, follow the call chain to the end, and answer CONFIRMED or REJECTED.\n\n{}", VERIFIER_PROMPT.replace("{claims}", &claim_lines(&batch)));
        let reply = ask(config, &prompt, role, 30, config.review_pipeline_tokens, Some(vec!["code-review".into()]), None, "never").await;
        let mut tmp = batch;
        let n = tmp.len();
        apply_verdicts(&mut tmp, 0..n, &reply, config.review_min_confidence);
        for (k, i) in unclear.iter().enumerate() {
            let mut f = tmp[k].clone();
            f.same_as = 0; // numbering inside this second batch means nothing in the whole list
            if f.verdict == "UNCLEAR" {
                f.verdict = "PLAUSIBLE".into();
                f.evidence = "could not be settled by reading the code".into(); // only a proven refutation drops a claim
            }
            findings[*i] = f;
        }
    }
}

/// Run the confirmed HIGH findings in a throwaway copy of the change. A defect that shows up in output is the strongest
/// evidence there is; one the real code path refuses to show is capped at MEDIUM, with what was tried written next to it.
async fn reproduce(config: &Config, role: &str, cwd: &Path, findings: &mut [Finding], files: &[String], target: Option<&Target>) {
    let picks: Vec<usize> = (0..findings.len()).filter(|i| findings[*i].verdict == "CONFIRMED" && matches!(shown_severity(&findings[*i]), "CRITICAL" | "HIGH")).take(MAX_REPROS).collect();
    if picks.is_empty() {
        return;
    }
    let own = !target.map_or(false, |t| t.cwd.is_some()); // a PR worktree is already a throwaway
    let scratch: Option<PathBuf> = if own {
        let committed = target.map_or(false, |t| !t.includes_worktree());
        make_scratch(cwd, files, if committed { target.unwrap().rev.last().map(String::as_str).unwrap_or("HEAD") } else { "HEAD" }, !committed).await
    } else {
        let t = target.unwrap();
        let s = t.cwd.clone();
        if let Some(s) = &s {
            link_dependencies(t.repo.as_deref().unwrap_or(cwd), s);
        }
        s
    };
    let Some(scratch) = scratch else { return };
    let mut scfg = config.clone();
    scfg.cwd = scratch.clone();
    let futs = picks.iter().map(|i| {
        let f = &findings[*i];
        let prompt = REPRO_PROMPT.replace("{claim}", &format!("{} | {}", f.where_, f.what)).replace("{evidence}", if f.evidence.is_empty() { "(none)" } else { &f.evidence });
        let scfg = &scfg;
        async move { ask(scfg, &prompt, role, 20, config.review_pipeline_tokens, None, Some(REPRO_TOOLS.iter().map(|s| s.to_string()).collect()), "auto").await }
    });
    let replies = futures::future::join_all(futs).await;
    for (i, reply) in picks.iter().zip(replies) {
        let (word, rest) = parse_repro(&reply);
        let f = &mut findings[*i];
        if word == "REPRODUCED" {
            f.repro = format!("reproduced: {rest}");
        } else if word == "NOT_REPRODUCED" {
            f.repro = format!("tried to reproduce and could not: {rest}");
            if matches!(f.severity.as_str(), "CRITICAL" | "HIGH") {
                f.severity = "MEDIUM".into();
            }
        }
    }
    if own {
        drop_scratch(cwd, &scratch).await;
    }
}

/// (title, rest): the first sentence is the heading and the remainder the explanation.
fn headline(what: &str) -> (String, String) {
    let text = tidy(what, 700);
    if let Some(c) = Re::new(r"(?s)^(.{20,170}?[.!?])\s+(.*)", false).ok().and_then(|r| r.captures(&text)) {
        return (c[1].clone().unwrap_or_default().trim_end_matches('.').to_string(), c[2].clone().unwrap_or_default().trim().to_string());
    }
    if text.chars().count() <= 190 {
        return (text, String::new());
    }
    if let Some(c) = Re::new(r"(?s)^(.{40,170}?)(?:\s[-\x{2014}]+\s|;\s|:\s|,\s)(.*)", false).ok().and_then(|r| r.captures(&text)) {
        return (c[1].clone().unwrap_or_default(), c[2].clone().unwrap_or_default().trim().to_string());
    }
    let head: String = text.chars().take(170).collect();
    let cut = head.rsplit_once(' ').map_or(head.as_str(), |x| x.0).to_string();
    let rest = text.chars().skip(cut.chars().count()).collect::<String>().trim().to_string();
    (cut, rest)
}

fn sorted_live(findings: &[Finding]) -> Vec<Finding> {
    let mut live: Vec<Finding> = findings.iter().filter(|f| f.verdict != "REJECTED").cloned().collect();
    live.sort_by(|a, b| (a.verdict != "CONFIRMED", severity_rank(shown_severity(a)), a.where_.clone()).cmp(&(b.verdict != "CONFIRMED", severity_rank(shown_severity(b)), b.where_.clone())));
    live
}

fn groups_of(live: &[Finding]) -> [Vec<Finding>; 4] {
    let proven: Vec<Finding> = live.iter().filter(|f| f.verdict == "CONFIRMED").cloned().collect();
    let blocking: Vec<Finding> = proven.iter().filter(|f| matches!(shown_severity(f), "CRITICAL" | "HIGH")).cloned().collect();
    let rest: Vec<Finding> = proven.iter().filter(|f| shown_severity(f) == "MEDIUM").cloned().collect();
    let minor: Vec<Finding> = proven.iter().filter(|f| !matches!(shown_severity(f), "CRITICAL" | "HIGH" | "MEDIUM")).cloned().collect();
    let guess: Vec<Finding> = live.iter().filter(|f| f.verdict != "CONFIRMED").cloned().collect();
    [blocking, rest, minor, guess]
}

/// The findings in the order the answer numbers them (proven blockers, proven fixes, minor, then guesses). "Fix 2" means item 2.
fn display_order(findings: &[Finding]) -> Vec<Finding> {
    let live = sorted_live(findings);
    let [b, r, m, g] = groups_of(&live);
    [b, r, m, g].concat()
}

/// The review a user reads, built from the verified findings with no model call (markdown, which the terminal renders).
fn format_answer(findings: &[Finding]) -> String {
    let live = sorted_live(findings);
    let ruled = findings.iter().filter(|f| f.verdict == "REJECTED").count();
    if live.is_empty() {
        return format!("## Code review\n\nNo issues found.{}", if ruled > 0 { format!(" I checked {ruled} suspected problem(s) and the code ruled them out.") } else { String::new() });
    }
    let [blocking, rest, minor, guess] = groups_of(&live);
    let mut out = vec![format!("## Code review: {} issue{}", live.len(), if live.len() != 1 { "s" } else { "" })];
    let mut n = 0;
    for (title, group) in [("Likely bugs", &blocking), ("Worth fixing", &rest), ("Minor", &minor), ("Not verified, worth a look", &guess)] {
        if group.is_empty() {
            continue;
        }
        let mut lines = vec![format!("### {title}")];
        for f in group {
            n += 1;
            let (head, more) = headline(&f.what);
            lines.push(format!("**{n}. `{}`  {head}**", f.where_));
            if !more.is_empty() {
                lines.push(format!("- **Why:** {}.", more.trim_end_matches('.')));
            }
            if !f.reach.is_empty() && f.reach != "n/a" {
                lines.push(format!("- **Affects:** {}", tidy(&f.reach, 160)));
            }
            if !f.temporary.is_empty() {
                lines.push(format!("- **Temporary:** goes away by itself ({})", tidy(&f.temporary, 80)));
            }
            if !f.fix.is_empty() {
                lines.push(format!("- **Fix:** {}", tidy(&f.fix, 400).trim_end_matches('.')));
            }
            if !f.repro.is_empty() {
                lines.push(format!("- **Repro:** {}", tidy(&f.repro, 300)));
            }
            lines.push(String::new());
        }
        out.push(lines.join("\n").trim_end().to_string());
    }
    if !blocking.is_empty() {
        let k = blocking.len();
        out.push(format!("**Before merging:** fix {} {}. Want me to fix {}?", if k == 1 { "item" } else { "items" }, (1..=k).map(|i| i.to_string()).collect::<Vec<_>>().join(", "), if k == 1 { "it" } else { "them" }));
    } else {
        out.push("**None of these block the merge.** Want me to fix any of them?".into());
    }
    out.join("\n\n")
}

/// New input + output tokens of each sub-agent, in the order they finished.
fn spent_log() -> &'static Mutex<Vec<i64>> {
    static L: OnceLock<Mutex<Vec<i64>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(vec![]))
}

/// A path the reviewer wrote must exist. A near miss is mended when exactly one file has that name; anything else is
/// marked so the reader does not go looking for it.
async fn fix_paths(findings: &mut [Finding], cwd: &Path) {
    let tree = crate::agent::grounding::tree(cwd).await;
    for f in findings.iter_mut() {
        let (path, rest) = match f.where_.split_once(':') {
            Some((p, r)) => (p.to_string(), Some(r.to_string())),
            None => (f.where_.clone(), None),
        };
        if path.trim().is_empty() || crate::agent::grounding::resolve_in_tree(cwd, path.trim(), &tree).is_some() {
            continue;
        }
        let base = path.trim().rsplit('/').next().unwrap_or("").to_string();
        let same: Vec<&String> = tree.iter().filter(|t| t.rsplit('/').next().unwrap_or("") == base).collect();
        f.where_ = if same.len() == 1 { format!("{}{}", same[0], rest.map(|r| format!(":{r}")).unwrap_or_default()) } else { format!("{} (file not found)", f.where_) };
    }
}

/// One read-only sub-agent with its own context; returns its last text. A crash or a stop is an empty answer, never an error.
#[allow(clippy::too_many_arguments)]
pub async fn ask(config: &Config, prompt: &str, role: &str, max_turns: usize, max_tokens: usize, disabled_skills: Option<Vec<String>>, tools: Option<Vec<String>>, approval: &str) -> String {
    if config.review_backend == "claude-code" && crate::agent::review_claude::available() && tools.is_none() {
        // a repro needs a shell, which the read-only Claude sessions do not get
        let (answer, tokens) = crate::agent::review_claude::ask_claude(config, prompt, 240).await;
        if !answer.trim().is_empty() {
            spent_log().lock().unwrap().push(tokens);
            return answer;
        }
        // an empty answer (not logged in, timeout) falls through to a melon reviewer instead of losing the review
    }
    let mut cfg = config.clone();
    cfg.model_role = role.into();
    cfg.allowed_tools = Some(tools.unwrap_or_else(|| ["read_file", "grep", "glob", "list_dir", "graph", "search", "outline", "git", "use_skill"].iter().map(|s| s.to_string()).collect()));
    cfg.auto_review = false;
    cfg.auto_verify = false;
    cfg.verify_commands = vec![];
    cfg.verify_rules = vec![];
    cfg.subagents = vec![];
    cfg.enable_parallel_tasks = false;
    cfg.mcp_servers = Default::default();
    cfg.review_pipeline = false;
    cfg.auto_challenge = false;
    cfg.auto_scout = false;
    cfg.sub_agent = true;
    cfg.auto_route = false; // the prompt is a review brief, not a user request: its words must not send it to the strong tier
    cfg.max_turns = max_turns;
    cfg.max_total_tokens = max_tokens as i64;
    cfg.max_seconds = 300.0;
    cfg.approval = ApprovalPolicy::parse(approval).unwrap_or(ApprovalPolicy::Never);
    cfg.disabled_skills.extend(disabled_skills.unwrap_or_default());
    let wall = Duration::from_secs_f64(cfg.max_seconds + 60.0);
    // the agent's own 300s budget only covers model waits; a stall anywhere else must not hang the review
    match tokio::time::timeout(wall, crate::agent::agent::run_collect(cfg, prompt)).await {
        Ok(c) => {
            spent_log().lock().unwrap().push(c.spent);
            c.texts.last().cloned().unwrap_or_default()
        }
        Err(_) => String::new(),
    }
}

/// The project's own typecheck / lint / related specs for the changed files, run once and shared with every reviewer.
async fn run_checks(config: &Config, files: &[String]) -> Vec<CheckResult> {
    let mut out = vec![];
    for (cmd, sub) in select_checks(config, files) {
        out.push(run_check(&cmd, &config.cwd.join(sub), config).await);
    }
    out
}

/// A failed check that names a changed file is a proven defect with no model involved: `src/a.ts(12,5): error TS2322`.
/// A failure that names none of them (old debt elsewhere) is left for the frame to mention, not blamed on this change.
fn check_findings(results: &[CheckResult], files: &[String]) -> Vec<Finding> {
    let mut out = vec![];
    for r in results.iter().filter(|r| !r.ok) {
        for f in files {
            let Some(m) = Re::new(&format!(r"{}[(:](\d+)", regex::escape(f)), false).ok().and_then(|re| re.captures(&r.output_tail)) else { continue };
            let line = r.output_tail.lines().find(|l| l.contains(f.as_str())).unwrap_or("").trim().chars().take(160).collect::<String>();
            out.push(Finding {
                severity: "HIGH".into(),
                where_: format!("{f}:{}", m[1].clone().unwrap_or_default()),
                what: format!("`{}` fails here: {line}", r.command),
                fix: format!("make `{}` pass", r.command),
                verdict: "CONFIRMED".into(),
                evidence: format!("exit {} of {}", r.exit_code.map_or("none".into(), |c| c.to_string()), r.command),
                reach: "everyone who builds this".into(),
                ..Default::default()
            });
            break;
        }
    }
    out
}

pub enum PipelineEvent {
    Notice(String),
    Answer(String),
    Findings(Vec<Finding>),
    Results(String),
}

/// Resolve what to review, run it, and remove the throwaway worktree a PR from another branch was checked out in.
pub async fn run_review_pipeline(config: Config, request: String, tx: UnboundedSender<PipelineEvent>) {
    let target = review_target::resolve(&config.cwd, &request, SKIP_SUFFIXES).await;
    if target.is_none() && named_scope().is_match(&request) {
        return; // it names a commit/PR/branch we cannot read from here: the normal review answers
    }
    let mut config = config;
    if let Some(t) = target.as_ref().and_then(|t| t.cwd.clone()) {
        config.cwd = t; // reviewers, checks' rules and `git show` all read the PR's tree
    }
    run_review(&config, &request, target.as_ref(), &tx).await;
    if let Some(t) = &target {
        t.cleanup().await;
    }
}

/// Emits notices as it goes and finally the answer and result block, or nothing when there is nothing to review.
/// Reviewers each take one angle on the change, the project's checks run beside them, a last reviewer hunts for what
/// they all missed, then verifiers try to refute every claim and score it.
async fn run_review(config: &Config, request: &str, target: Option<&Target>, tx: &UnboundedSender<PipelineEvent>) {
    let say = |m: String| {
        let _ = tx.send(PipelineEvent::Notice(m));
    };
    let cwd = config.cwd.clone();
    let (mut moves, mut deleted) = (vec![], vec![]);
    let (mut files, rev, label): (Vec<String>, Vec<String>, String);
    if let Some(t) = target {
        files = t.files.clone();
        rev = t.rev.clone();
        label = t.label.clone();
    } else {
        let porcelain = git(&cwd, &["status", "--porcelain", "-uall"]).await; // -uall: a new folder is listed file by file
        let added = changed_files(&porcelain);
        let (m, d, f) = pair_moves(&cwd, &deleted_files(&porcelain), &added).await;
        moves = m;
        deleted = d;
        files = f;
        files.extend(deleted.iter().cloned()); // build_diff shows a deletion as what was removed
        rev = vec!["HEAD".to_string()];
        label = "the whole uncommitted change".into();
    }
    if files.is_empty() && moves.is_empty() {
        return;
    }
    let whole = build_diff(&cwd, &files, &rev).await;
    // a diff past the 40k prompt cap is still one reviewer's job (it reads the rest with `git diff`); only a really wide
    // change (30+ files) is cut into areas, and then each area gets one all-angle reviewer, not four
    let groups = if whole.contains("[diff clipped]") && files.len() > SPLIT_FILES { split_clipped(&files) } else { vec![("the whole change".to_string(), files.clone())] };
    if let Some(t) = target {
        for n in &t.notes {
            say(format!("WARNING scope: {n}"));
        }
        if !t.commits.is_empty() {
            say(format!("Commits covered: {}{}", t.commits.iter().take(6).cloned().collect::<Vec<_>>().join("; "), if t.commits.len() > 6 { format!("; ... ({} in all)", t.commits.len()) } else { String::new() }));
        }
    }
    say(format!("Reviewing {} changed file(s){}{}", files.len(), if target.is_some() { format!(" in {label}") } else { String::new() }, if moves.is_empty() { String::new() } else { format!(" and {} moved unchanged", moves.len()) }));
    let role = config.review_pipeline_role.clone();
    let stayed = left_behind(&cwd, &moves).await;
    spent_log().lock().unwrap().clear();

    let want_checks = config.review_run_checks && target.map_or(true, |t| t.includes_worktree() && t.cwd.is_none()); // a fresh PR worktree has no installed dependencies
    let check_task = want_checks.then(|| {
        let (c, f) = (config.clone(), files.clone());
        tokio::spawn(async move { run_checks(&c, &f).await })
    });
    let mut checks: Vec<CheckResult> = vec![];
    let mut late: Vec<CheckResult> = vec![];
    if let Some(task) = check_task {
        let mut task = task;
        match tokio::time::timeout(Duration::from_secs(CHECK_WAIT), &mut task).await {
            Ok(Ok(r)) => {
                checks = r;
                late = checks.clone();
            }
            Ok(Err(_)) => {}
            Err(_) => {
                say(format!("Checks are still running after {CHECK_WAIT}s; the reviewers start without them"));
                task.abort();
            }
        }
        if !checks.is_empty() {
            say(format!("Checks: {}", checks.iter().map(|c| format!("{} {}", c.command, if c.ok { "ok" } else { "FAILED" })).collect::<Vec<_>>().join(", ")));
        }
    }
    let checked = if checks.is_empty() {
        String::new()
    } else {
        format!("AUTOMATED CHECKS ALREADY RUN (a failure naming a changed file is already reported; do not repeat it, do not re-run them): {}\n\n", checks.iter().map(|c| format!("{}={}", c.command, if c.ok { "ok" } else { "FAILED" })).collect::<Vec<_>>().join("; "))
    };
    let mut context = format!("{checked}{}{}", review_memory::render(&cwd, None), review_context(config, request, &files));
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    let secure = security_hint().is_match(&format!("{}\n{whole}", files.join(" ")));
    let mut lenses = if groups.len() == 1 { pick_lenses(config, &whole, &files, &context) } else { vec![one_reviewer(&context, secure)] };
    let mut light = false;
    if config.review_lenses == 0 && groups.len() == 1 && is_light(&whole, &files, secure) {
        light = true;
        let (n, t) = one_reviewer(&context, false);
        lenses = vec![(n, format!("{t}{LIGHT_NOTE}"))];
        if target.map_or(true, |t| t.includes_worktree()) {
            let around = changed_regions(&cwd, &whole);
            if !around.is_empty() {
                context.push_str(&format!("SURROUNDING CODE (the current text around each changed hunk; you do not need to read these again):\n{around}\n\n"));
            }
        }
        say("Small change: one quick reviewer, no triage and no verifier".into());
    } else if config.review_lenses == 0 && groups.len() == 1 {
        let picks = triage(config, &role, &whole, &files, &context).await;
        if !picks.is_empty() {
            lenses = triage_lenses(&picks);
            say(format!("Triage: {} thing(s) worth checking, {} reviewer(s) assigned", picks.len(), lenses.len()));
        } else {
            say("Triage found nothing specific; one reviewer reads the whole change".into());
            lenses = vec![one_reviewer(&context, secure)];
        }
    }
    let clipped = whole.contains("[diff clipped]"); // a split diff already multiplies the reviewers
    if groups.len() * lenses.len() > 1 {
        let angles = lenses.iter().map(|(n, _)| if n == "review" { "every angle".to_string() } else { n.clone() }).collect::<Vec<_>>().join(", ");
        say(format!(
            "{} reviewers in parallel{}{}",
            groups.len() * lenses.len(),
            if groups.len() > 1 { format!(", one per part ({} parts)", groups.len()) } else { format!(": {angles}") },
            if context.contains("PROJECT RULES") || context.contains("PLAYBOOKS") { " (rules and playbooks loaded)" } else { "" }
        ));
    }
    if config.review_backend == "claude-code" {
        say("Reviewers run as Claude Code sessions (read-only)".into());
    }

    struct Out {
        findings: Option<Vec<Finding>>,
        summaries: Vec<String>,
        strengths: Vec<String>,
    }
    let review = |name: String, fs: Vec<String>, lens: String, turns: usize, groups_n: usize| {
        let (whole, cwd, rev, label, request, context, files) = (whole.clone(), cwd.clone(), rev.clone(), label.clone(), request.to_string(), context.clone(), files.clone());
        let (moves, stayed, role) = (moves.clone(), stayed.clone(), role.clone());
        let cfg: &Config = config;
        async move {
            let diff = if groups_n == 1 { whole } else { build_diff(&cwd, &fs, &rev).await };
            let others: String = files.iter().filter(|f| !fs.contains(f)).cloned().collect::<Vec<_>>().join(", ").chars().take(600).collect();
            let others = if others.is_empty() { "(none)".to_string() } else { others };
            let mut scope = if groups_n == 1 { label.clone() } else { format!("one part ({name}) of {label} that {groups_n} reviewers split") };
            if clipped && groups_n == 1 {
                scope.push_str(&format!(" (the diff below is cut short: for the files it does not reach, run `git diff {} -- <file>` and read each one before you finish)", rev[0]));
            }
            if rev.len() > 1 {
                scope.push_str(&format!(" (a committed range: read old code with `git show {}:<file>` and new code with `git show {}:<file>`)", rev[0], rev[1]));
            }
            let moved = if moves.is_empty() {
                String::new()
            } else {
                format!(
                    "MOVED WITHOUT ANY CHANGE (identical bytes, so there is nothing in them to review; check that imports and paths still resolve): {}\nSTILL IN THE OLD FOLDER (did any of these rely on what moved, e.g. inherit its loading.tsx, layout or index export? say what they lose, or that they are fine): {}\n",
                    moves.iter().map(|(a, b)| format!("{a} -> {b}")).collect::<Vec<_>>().join("; "),
                    if stayed.is_empty() { "(nothing)".to_string() } else { stayed.join(", ") }
                )
            };
            let prompt = REVIEWER_PROMPT
                .replace("{scope}", &scope)
                .replace("{request}", &request.chars().take(1500).collect::<String>())
                .replace("{part}", &fs.iter().take(25).cloned().collect::<Vec<_>>().join(", "))
                .replace("{moved}", &moved)
                .replace("{others}", &others)
                .replace("{context}", &context)
                .replace("{diff}", &diff)
                .replace("{lens}", &lens);
            let reply = ask(cfg, &prompt, &role, turns, cfg.review_pipeline_tokens, None, None, "never").await;
            let grab = |tag: &str| -> Vec<String> {
                let re = lit_i(&format!(r"(?m)^\W*{tag}:\s*(.+)$"));
                let (mut v, mut at) = (vec![], 0);
                while at < reply.len() {
                    let Some(c) = re.captures(&reply[at..]) else { break };
                    let (_, e) = re.find(&reply[at..]).unwrap();
                    v.extend(c[1].clone());
                    at += e.max(1);
                }
                v
            };
            Out { summaries: grab("SUMMARY"), strengths: grab("STRENGTH"), findings: (!reply.trim().is_empty()).then(|| parse_findings(&reply)) }
        }
    };
    let jobs: Vec<_> = groups
        .iter()
        .flat_map(|(n, fs)| lenses.iter().map(move |(lname, lens)| (n.clone(), fs.clone(), lname.clone(), lens.clone())))
        .map(|(n, fs, lname, lens)| {
            let turns = if light { LIGHT_TURNS } else if lname == "review" || lname == "contracts" { 30 } else { 20 };
            review(n, fs, lens, turns, groups.len())
        })
        .collect();
    let outs = futures::future::join_all(jobs).await;
    let mut review_tokens: i64 = spent_log().lock().unwrap().iter().sum();
    if outs.iter().all(|o| o.findings.is_none()) {
        say("The reviewer gave no answer; reviewing without the pipeline".into());
        return; // an empty answer is a failure, not "no defects": the normal review loop takes over
    }
    let (mut summaries, mut strengths): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let mut found: Vec<Finding> = vec![];
    for o in outs {
        summaries.extend(o.summaries);
        strengths.extend(o.strengths);
        found.extend(o.findings.unwrap_or_default());
    }
    found.sort_by_key(|f| severity_rank(&f.severity));
    found = merge_duplicates(found);

    if config.review_gap_pass && groups.len() == 1 && lenses.len() >= 2 && whole.matches('\n').count() >= GAP_MIN_LINES {
        say("A last reviewer hunts for what the others missed".into());
        let reported = if found.is_empty() { "(nothing)".to_string() } else { found.iter().map(|f| format!("- {} {} {}", f.severity, f.where_, f.what)).collect::<Vec<_>>().join("\n") };
        let o = review("the whole change".into(), files.clone(), GAP_LENS.replace("{found}", &reported), 20, groups.len()).await;
        summaries.extend(o.summaries);
        strengths.extend(o.strengths);
        found.extend(o.findings.unwrap_or_default());
        found.sort_by_key(|f| severity_rank(&f.severity));
        found = merge_duplicates(found);
        review_tokens = spent_log().lock().unwrap().iter().sum();
    }

    fix_paths(&mut found, &cwd).await;
    found = drop_missing_files(found, &files);
    let extra = if found.len() > MAX_VERIFIED { found.split_off(MAX_VERIFIED) } else { vec![] };
    let mut to_check = found;
    if !to_check.is_empty() && !light {
        // a quick pass on a small change is shown as the reviewer's claims, marked unverified, like Claude Code's low effort
        say(format!("Verifying {} finding(s); the verifier tries to refute and score each", to_check.len()));
        verify(config, &role, &mut to_check).await;
    }
    let mut found = merge_same_cause(to_check);
    found.extend(extra);
    if config.review_repro && found.iter().any(|f| f.verdict == "CONFIRMED") {
        say("Running the confirmed defects in a throwaway copy to see them happen".into());
        reproduce(config, &role, &cwd, &mut found, &files, target).await;
    }
    let mut with_checks = check_findings(&late, &files); // a failing typecheck on a changed file needs no model to confirm
    with_checks.extend(found);
    let found = with_checks;
    let ruled_out: Vec<(String, String, String)> = found
        .iter()
        .filter(|f| f.verdict == "REJECTED" && !["low confidence", "already present", "could not be settled"].iter().any(|p| f.evidence.starts_with(p)))
        .map(|f| (f.where_.clone(), f.what.clone(), f.evidence.clone()))
        .collect();
    review_memory::remember(&cwd, &ruled_out, None);
    let confirmed = found.iter().filter(|f| f.verdict == "CONFIRMED").count();
    let rejected = found.iter().filter(|f| f.verdict == "REJECTED").count();
    let verify_tokens = spent_log().lock().unwrap().iter().sum::<i64>() - review_tokens;
    say(format!("Verified: {confirmed} confirmed, {rejected} rejected, {} unverified", found.len() - confirmed - rejected));
    say(format!("Tokens: reviewers {review_tokens} + verifier {verify_tokens} = {}", review_tokens + verify_tokens));
    let scope = match target {
        Some(t) => format!("{}{}.", t.label, if t.notes.is_empty() { String::new() } else { format!(" ({})", t.notes.join("; ")) }),
        None => String::new(),
    };
    let _ = tx.send(PipelineEvent::Findings(display_order(&found)));
    let answer = frame(&format_answer(&found), summaries.first().map(|s| s.trim()).unwrap_or(""), &files, &moves, &deleted, &late, &strengths.iter().take(2).cloned().collect::<Vec<_>>(), &scope);
    let _ = tx.send(PipelineEvent::Answer(answer));
    let _ = tx.send(PipelineEvent::Results(compose(&found, groups.len() * lenses.len())));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(sev: &str, v: &str) -> Finding {
        Finding { severity: sev.into(), where_: "a.ts:1".into(), what: "The insert can throw and 500 the whole list. More detail follows here".into(), fix: "catch it".into(), verdict: v.into(), ..Default::default() }
    }

    #[test]
    fn findings_and_verdicts() {
        let found = parse_findings("- HIGH | a.ts:4 | thing breaks | catch it\nNONE\nSUMMARY: x\n2. medium | b.ts:1 | other | ");
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].severity, "MEDIUM");
        let v = parse_verdicts("1 | CONFIRMED | a.ts:4 read it | all users | TEMPORARY (15 min) | NEW | NONE | CONF 80\n3 | REJECTED | x | n/a | ONGOING | ALREADY_AT_HEAD | SAME_AS 1 | CONF 10", 3);
        assert_eq!((v[0].0.as_str(), v[0].3.as_str(), v[0].6), ("CONFIRMED", "15 min", 80));
        assert_eq!(v[1].0, "UNCLEAR");
        assert!(v[2].4 && v[2].5 == 1);
    }

    #[test]
    fn severity_is_earned() {
        assert_eq!(shown_severity(&f("HIGH", "PLAUSIBLE")), "MEDIUM");
        assert_eq!(shown_severity(&f("HIGH", "CONFIRMED")), "HIGH");
        let mut t = f("HIGH", "CONFIRMED");
        t.temporary = "15 min".into();
        assert_eq!(shown_severity(&t), "MEDIUM");
    }

    #[test]
    fn verdicts_apply_and_merge() {
        let mut fs = vec![f("HIGH", "UNCHECKED"), f("HIGH", "UNCHECKED")];
        fs[1].where_ = "b.ts:2".into();
        apply_verdicts(&mut fs, 0..2, "1 | CONFIRMED | e | r | ONGOING | NEW | NONE | CONF 90\n2 | CONFIRMED | e | r | ONGOING | NEW | SAME_AS 1 | CONF 90", 60);
        let merged = merge_same_cause(fs);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].where_, "a.ts:1, b.ts:2");
        let mut low = vec![f("HIGH", "UNCHECKED")];
        apply_verdicts(&mut low, 0..1, "1 | CONFIRMED | e | r | ONGOING | NEW | NONE | CONF 20", 60);
        assert_eq!(low[0].verdict, "REJECTED");
    }

    #[test]
    fn answer_layout() {
        let a = format_answer(&[f("HIGH", "CONFIRMED"), f("MEDIUM", "PLAUSIBLE")]);
        assert!(a.starts_with("## Code review: 2 issues"));
        assert!(a.contains("### Likely bugs") && a.contains("### Not verified, worth a look"));
        assert!(a.contains("**1. `a.ts:1`  The insert can throw and 500 the whole list**"));
        assert!(a.contains("Want me to fix it?"));
        assert!(format_answer(&[]).contains("No issues found"));
        assert_eq!(display_order(&[f("MEDIUM", "PLAUSIBLE"), f("HIGH", "CONFIRMED")])[0].verdict, "CONFIRMED");
    }

    #[test]
    fn grouping_and_moves_of_text() {
        let files: Vec<String> = ["apps/web/a.ts", "apps/web/b.ts", "apps/server/c.ts", "docs/d.md", "x.md"].iter().map(|s| s.to_string()).collect();
        let g = group_files(&files, 3);
        assert_eq!(g.len(), 3);
        assert!(g[2].0.contains('+'));
        assert_eq!(changed_files(" M a.ts\n D b.ts\n?? c.png\n?? d.ts"), vec!["a.ts", "d.ts"]);
        assert_eq!(parse_triage("CHECK | a.ts:1 | who reads it\nnothing"), vec!["a.ts:1: who reads it"]);
        assert_eq!(tidy("short thing.<tool_call>junk", 100), "short thing");
        let (h, r) = headline("The insert can throw and 500 the whole list. More detail follows here");
        assert_eq!((h.as_str(), r.as_str()), ("The insert can throw and 500 the whole list", "More detail follows here"));
    }
}
