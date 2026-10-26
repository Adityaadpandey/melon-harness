use crate::agent::skills::{extract_section, find_in_repo, headings, load_skills, SECTION_CAP};
use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

pub struct UseSkillTool {
    config: ConfigHandle,
}
impl UseSkillTool {
    pub fn new(config: ConfigHandle) -> Self {
        UseSkillTool { config }
    }
}

#[async_trait]
impl Tool for UseSkillTool {
    fn name(&self) -> &str {
        "use_skill"
    }
    fn description(&self) -> String {
        "Load a project playbook (rules, runbooks, checklists). With no section it returns the distilled playbook; with `section` it returns that part of the full reference document.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(
            json!({
                "name": {"type": "string", "description": "Skill name from the Skills index"},
                "section": {"type": ["string", "null"], "description": "Optional. 'list' shows the headings of the skill's full reference document; any other value returns the section whose heading contains that text."},
            }),
            &["name"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let cfg = self.config.get();
        let skills = load_skills(&cfg);
        let name = inv.str("name").unwrap_or("");
        let Some(skill) = skills.iter().find(|s| s.name == name) else {
            return ToolResult::error(format!("Unknown skill '{name}'. Available: {}", if skills.is_empty() { "none".into() } else { skills.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ") }));
        };
        let Some(section) = inv.str("section").filter(|s| !s.is_empty()) else {
            // a folder skill points at its own files ("see references/forms.md"); without the folder the model guesses paths
            if let Some(p) = skill.path.as_ref().filter(|p| p.file_name().map_or(false, |n| n == "SKILL.md")) {
                return ToolResult::ok(format!("Base directory for this skill: {}\n\n{}", p.parent().unwrap().display(), skill.body));
            }
            return ToolResult::ok(skill.body.clone());
        };
        let Some(canonical) = &skill.canonical else { return ToolResult::error(format!("Skill '{name}' has no reference document; use it without `section`.")) };
        let Some(doc) = find_in_repo(&inv.cwd, canonical) else { return ToolResult::error(format!("Reference document {canonical} not found from {}", inv.cwd.display())) };
        let text = std::fs::read_to_string(&doc).unwrap_or_default();
        if section.trim().to_lowercase() == "list" {
            let rows: Vec<String> = headings(&text).into_iter().filter(|(_, lvl, _)| *lvl <= 3).map(|(ln, lvl, title)| format!("{}{title}  (line {ln})", "  ".repeat(lvl - 1))).collect();
            return ToolResult::ok(format!("Sections of {canonical}:\n{}", rows.join("\n")));
        }
        let Some(sec) = extract_section(&text, section) else { return ToolResult::error(format!("No section matching '{section}'. Use section='list' to see headings.")) };
        let clipped = sec.chars().count() > SECTION_CAP;
        let mut r = ToolResult::ok(format!("{}{}", sec.chars().take(SECTION_CAP).collect::<String>(), if clipped { "\n... [section clipped]" } else { "" }));
        r.truncated = clipped;
        r
    }
}
