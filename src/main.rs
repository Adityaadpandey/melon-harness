#![allow(dead_code)] // kept for parity with the Python modules; some helpers are not used by any mode yet
mod agent;
mod client;
mod config;
mod context;
mod hooks;
mod prompts;
mod safety;
mod setup;
mod tools;
mod ui;
mod util;
#[cfg(test)]
mod tests;

use clap::Parser;
use config::loader::{load_config, load_last_model, save_last_model, LoadOptions};
use config::ApprovalPolicy;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use ui::app::Cli;
use ui::style::*;

/// zerd: a small coding agent for real repos.
#[derive(Parser, Debug)]
#[command(name = "zerd", version, about)]
struct Args {
    /// What to do (omit for the interactive session)
    prompt: Option<String>,
    /// Current working directory
    #[arg(short = 'c', long)]
    cwd: Option<PathBuf>,
    /// Let a planner split the request into parallel workers
    #[arg(long)]
    parallel: bool,
    /// Task graph JSON (implies --parallel)
    #[arg(long)]
    graph: Option<PathBuf>,
    /// Fast-forward the integrated result into a clean checkout
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    apply: bool,
    /// Print agent events as JSON lines instead of the UI (needs a prompt)
    #[arg(long = "json")]
    json_mode: bool,
    /// Read JSON commands on stdin, write events on stdout (for editors and scripts)
    #[arg(long = "rpc")]
    rpc_mode: bool,
    /// Write run metrics JSON here
    #[arg(long)]
    report: Option<PathBuf>,
    /// Extra directory the agent may use besides the working directory (repeatable)
    #[arg(long = "add-dir")]
    add_dirs: Vec<PathBuf>,
    /// Continue a saved session by id (shown when you exit; /sessions lists them)
    #[arg(long = "resume")]
    resume_id: Option<String>,
    /// Continue the newest saved session that ran in this folder
    #[arg(long = "continue")]
    continue_last: bool,
    /// Trust this project's .zerd/config.toml (hooks, verify commands, approval)
    #[arg(long = "trust")]
    trust_project: bool,
    /// Have a second model argue against risky requests first (always on interactively)
    #[arg(long = "challenge")]
    challenge_mode: bool,
    /// Reasoning effort (Codex models)
    #[arg(long, value_parser = ["low", "medium", "high"])]
    effort: Option<String>,
    /// Model id (overrides config)
    #[arg(short = 'm', long)]
    model: Option<String>,
    /// Pi mode on for this run (overrides pi_mode in config)
    #[arg(long, conflicts_with = "no_pi")]
    pi: bool,
    /// Pi mode off for this run
    #[arg(long = "no-pi")]
    no_pi: bool,
    /// Approval policy (overrides config)
    #[arg(long, value_parser = ["on-request", "on-failure", "auto", "auto-edit", "never", "yolo", "plan"])]
    approval: Option<String>,
}

fn scripted_early(a: &Args) -> bool {
    a.prompt.is_some() || a.json_mode || a.rpc_mode
}

fn warn(msg: &str) {
    eprintln!("{}", paint(msg, AMBER));
}

fn fail(msg: &str) -> ! {
    eprintln!("{}", bold_paint(msg, CORAL));
    std::process::exit(1);
}

fn main() {
    // subcommands that are not a request: zerd learn-import ..., zerd bench ...
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some("learn-import") {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        let argv: Vec<String> = std::iter::once("zerd learn-import".to_string()).chain(raw.into_iter().skip(2)).collect();
        std::process::exit(rt.block_on(agent::learn_import::main(argv)));
    }
    if raw.get(1).map(String::as_str) == Some("graph") {
        // `zerd graph callers parse_invoice` asks the graph tool directly, so you can see what the model would see
        const ASK: &[&str] = &["trace", "symbol", "find", "callers", "callees", "impact", "importers", "imports", "blast", "tests", "neighbors", "path", "community", "stats"];
        if raw.get(2).map_or(false, |a| ASK.contains(&a.as_str())) {
            use tools::Tool;
            let cwd = std::env::current_dir().unwrap_or_default();
            let inv = tools::ToolInvocation { params: serde_json::json!({"action": raw[2], "name": raw.get(3), "to": raw.get(4), "direction": raw.get(4), "depth": raw.get(5).and_then(|d| d.parse::<u64>().ok())}), cwd, epoch: 0, call_id: String::new(), progress: None };
            let rt = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
            let r = rt.block_on(tools::builtin::graph::GraphTool::new(config::ConfigHandle::new(config::Config::default())).execute(&inv));
            println!("{}", r.error.unwrap_or(r.output));
            std::process::exit(0);
        }
        std::process::exit(util::graphcfg::main(raw.into_iter().skip(2).collect()));
    }
    if matches!(raw.get(1).map(String::as_str), Some("setup" | "doctor")) {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        let cfg = load_config(None, &LoadOptions { trust_project: false, pi_mode: None }).unwrap_or_else(|e| fail(&format!("Configuration Error: {e}")));
        let code = if raw[1] == "setup" { if rt.block_on(setup::setup(&cfg, false)) { 0 } else { 1 } } else { rt.block_on(setup::doctor(&cfg)) };
        std::process::exit(code);
    }
    if raw.get(1).map(String::as_str) == Some("insights") {
        std::process::exit(util::insights::main(raw.into_iter().skip(2).collect()));
    }
    if raw.get(1).map(String::as_str) == Some("probe-models") {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        std::process::exit(rt.block_on(client::probe::main(raw.into_iter().skip(2).collect())));
    }
    let args = Args::parse();
    // a single-threaded runtime would starve the spinner thread's lock and the blocking prompts; four workers is plenty
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("runtime");
    let code = rt.block_on(run(args));
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    std::process::exit(code);
}

async fn run(args: Args) -> i32 {
    let pi_mode = if args.pi { Some(true) } else if args.no_pi { Some(false) } else { None };
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let one_shot = args.prompt.is_some();
    let mut config = match load_config(args.cwd.as_deref(), &LoadOptions { trust_project: args.trust_project, pi_mode }) {
        Ok(c) => c,
        Err(e) => fail(&format!("Configuration Error: {e}")),
    };
    if !config.ignored_project_keys.is_empty() {
        let keys = config.ignored_project_keys.join(", ");
        if tty && !one_shot {
            warn(&format!("This project's .zerd/config.toml sets: {keys}"));
            eprintln!("{}", paint("These can run commands or relax approvals. They are ignored unless you trust the file.", MUTED));
            eprint!("Trust it? [y/N] ");
            let _ = std::io::stderr().flush();
            let mut a = String::new();
            let _ = std::io::stdin().read_line(&mut a);
            if matches!(a.trim().to_lowercase().as_str(), "y" | "yes") {
                if let Ok(c) = load_config(args.cwd.as_deref(), &LoadOptions { trust_project: true, pi_mode }) {
                    config = c;
                }
            }
        } else {
            warn(&format!("Ignoring untrusted project settings: {keys} (use --trust to enable)"));
        }
    }
    for d in &args.add_dirs {
        match std::fs::canonicalize(d) {
            Ok(p) => config.allowed_dirs.push(p.to_string_lossy().to_string()),
            Err(_) => fail(&format!("--add-dir: {} is not a directory", d.display())),
        }
    }
    // First launch with nothing to call: walk through setup instead of starting a session that can only fail.
    if tty && !scripted_early(&args) && config.missing_key(None).is_some() && load_last_model().is_none() && std::env::var_os("ZERD_NO_SETUP").is_none() {
        if setup::setup(&config, true).await {
            config = load_config(args.cwd.as_deref(), &LoadOptions { trust_project: args.trust_project, pi_mode }).unwrap_or(config);
        }
    }
    // Interactive only: start on the model used last time. One-shot runs (benchmarks) keep the configured routing and
    // never touch the remembered model.
    let mut model = args.model.clone();
    if !one_shot {
        model = model.or_else(load_last_model);
    }
    if let Some(m) = model {
        config.model.name = m.clone();
        config.model_pinned = true;
        if !one_shot {
            save_last_model(&m);
        }
    }
    if let Some(e) = &args.effort {
        config.reasoning_effort = Some(e.clone());
    }
    let scripted = one_shot || args.json_mode || args.rpc_mode;
    config.auto_challenge = args.challenge_mode || !scripted;
    config.auto_learn = !scripted;
    config.verify_on_demand = !scripted;
    if let Some(a) = args.approval.as_deref().and_then(ApprovalPolicy::parse) {
        config.approval = a;
    } else if one_shot && config.approval == ApprovalPolicy::OnRequest {
        // A one-shot run has nobody to ask, so the default would refuse every edit. Let it edit files in the
        // workspace; commands still need an explicit --approval auto.
        config.approval = ApprovalPolicy::AutoEdit;
    }
    let mut errors = config.validate();
    if let Some(no_key) = config.missing_key(None) {
        if scripted {
            errors.push(no_key);
        } else {
            warn(&no_key); // interactive: start anyway, so /connect can fix it from inside
        }
    }
    if !errors.is_empty() {
        for e in errors {
            eprintln!("{}", bold_paint(&e, CORAL));
        }
        return 1;
    }

    let cwd = config.cwd.clone();
    if tty && !scripted {
        if let Some(f) = config::models_file::write_if_new(&config) {
            println!("{}", paint(&format!("◆ wrote {}: which model does which job (all commented out; edit to change)", f.strip_prefix(&cwd).unwrap_or(&f).display()), MUTED));
        }
    }
    let mut cli = Cli::new(config);
    let mut resume_id = args.resume_id.clone();
    if args.continue_last && resume_id.is_none() {
        resume_id = agent::persistence::PersistenceManager::new().latest_for(&cwd.to_string_lossy());
        if resume_id.is_none() {
            eprintln!("{}", paint("No saved session for this folder; starting a new one.", MUTED));
        }
    }
    cli.resume_id = resume_id;

    if (args.parallel || args.graph.is_some()) && one_shot {
        let ok = cli.run_orchestrated(args.prompt.as_deref().unwrap(), args.graph.as_deref(), args.apply, args.report.as_deref()).await;
        return if ok { 0 } else { 1 };
    }
    if args.rpc_mode {
        cli.run_rpc().await;
        return 0;
    }
    if args.json_mode {
        let Some(p) = &args.prompt else { fail("--json needs a prompt") };
        return if cli.run_json(p, args.report.as_deref()).await { 0 } else { 1 };
    }
    if let Some(p) = &args.prompt {
        let result = cli.run_single(p, args.report.as_deref()).await;
        let bad = matches!(cli.last_report.as_ref().and_then(|r| r["status"].as_str()), Some("failed" | "verification_failed"));
        return if result.is_none() || bad { 1 } else { 0 };
    }
    if tty && util::symgraph::listed(&cwd).len() >= 15 {
        // the symbol graph builds on every core while we get started; a new repo uses that wait to ask about itself
        util::symgraph::warm(cwd.clone());
        println!("{}", paint("◆ indexing the code graph in the background", MUTED));
        if !cwd.join(".zerd/graph.toml").exists() {
            util::graphcfg::first_run(&cwd);
        }
    }
    // a terminal tab names the running program; set a title the way Claude Code does, e.g. "✦ zerd · tunyt"
    if std::io::stdout().is_terminal() {
        let title = format!("✦ zerd · {}", cwd.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        print!("\x1b[22;0t\x1b]0;{title}\x07\x1b]1;{title}\x07\x1b]2;{title}\x07");
        let _ = std::io::stdout().flush();
    }
    cli.run_interactive().await;
    if std::io::stdout().is_terminal() {
        print!("\x1b]0;\x07\x1b]1;\x07\x1b]2;\x07\x1b[23;0t");
        let _ = std::io::stdout().flush();
    }
    0
}
