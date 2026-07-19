//! `familyclaw-actions-cli` — a thin operator command line for the action runtime.
//!
//! The binary is **just a shell** over the [`familyclaw_actions::ActionRuntime`]
//! facade (Layer A). It contains no action logic of its own and never touches
//! the pipeline's internals — all work is done through the facade.
//!
//! ## Commands
//! ```text
//! actions list-skills            registered skills + risk class (no secrets)
//! actions submit-task <json>     submit a task, print the task id
//! actions approve <approval-id>  consume/mark an approval → continue execution
//! actions status <task-id>       print the task's status
//! actions proof <task-id>        print the redacted proof bundle (JSON)
//! actions pending                print the pending approvals
//! ```
//!
//! `submit-task <json>` takes a JSON object of the form
//! `{ "skill_id": "<uuid>", "payload": { ... } }` or, alternatively,
//! `{ "skill": "<name>", "payload": { ... } }`, in which case the skill is looked up by name.
//!
//! ## Security
//! The output never contains secrets: the skill list shows only public
//! fields, and the proof bundle was already redacted in the pipeline. The
//! binary reads the clock only once at the I/O boundary (`now`) and injects
//! it into the facade.

use std::process::ExitCode;

use serde_json::Value;

use familyclaw_actions::facade::ActionRuntime;
use familyclaw_actions::ids::{ApprovalId, SkillId};
use familyclaw_core::time::{now, Timestamp};

/// Command-line entry point: parses arguments, runs the command, and returns
/// the process's exit code (`0` = success, `1` = error, `2` = wrong usage).
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: tokio-ajoympäristön luonti epäonnistui: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(&args))
}

/// Runs the selected subcommand. Separated from [`main`] so that the async
/// logic is testable and error handling is centralized.
async fn run(args: &[String]) -> ExitCode {
    let Some(command) = args.first().map(String::as_str) else {
        usage();
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    let injected_now = now();

    match command {
        "list-skills" => cmd_list_skills(),
        "submit-task" => cmd_submit_task(rest, injected_now).await,
        "approve" => cmd_approve(rest, injected_now).await,
        "status" => cmd_status(rest).await,
        "proof" => cmd_proof(rest),
        "pending" => cmd_pending(),
        "help" | "-h" | "--help" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: tuntematon komento '{other}'");
            usage();
            ExitCode::from(2)
        }
    }
}

/// Builds the runtime with the default skills. Prints an explanation on error.
fn build_runtime() -> Option<ActionRuntime> {
    match ActionRuntime::with_default_skills() {
        Ok(rt) => Some(rt),
        Err(e) => {
            eprintln!("error: ajoympäristön alustus epäonnistui: {e}");
            None
        }
    }
}

/// `list-skills`: prints the registered skills (id, name, version, risk,
/// approval requirement) as a single JSON array. No secrets.
fn cmd_list_skills() -> ExitCode {
    let Some(runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };
    let skills = runtime.list_skills();
    print_json_or_fail(&skills)
}

/// `submit-task <json>`: submits a task and runs the pipeline; prints the
/// task's id, status, and the id of the pending approval, if any.
async fn cmd_submit_task(rest: &[String], now: Timestamp) -> ExitCode {
    let Some(raw) = rest.first() else {
        eprintln!("error: submit-task vaatii JSON-argumentin");
        eprintln!(
            "  esim: actions submit-task '{{\"skill\":\"email_triage_mock\",\"payload\":{{}}}}'"
        );
        return ExitCode::from(2);
    };

    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: argumentti ei ole kelvollista JSONia: {e}");
            return ExitCode::from(2);
        }
    };

    let Some(mut runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };

    let payload = parsed.get("payload").cloned().unwrap_or(Value::Null);
    let skill_id = match resolve_skill_id(&runtime, &parsed) {
        Ok(id) => id,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };

    match runtime.submit_task(skill_id, payload, now).await {
        Ok(outcome) => print_json_or_fail(&outcome),
        Err(e) => {
            eprintln!("error: tehtävän lähetys epäonnistui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `approve <approval-id>`: consumes the approval and runs the task to completion.
async fn cmd_approve(rest: &[String], now: Timestamp) -> ExitCode {
    let Some(raw) = rest.first() else {
        eprintln!("error: approve vaatii hyväksynnän tunnisteen (uuid)");
        return ExitCode::from(2);
    };
    let approval_id: ApprovalId = match raw.parse() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: kelvoton hyväksynnän tunniste: {e}");
            return ExitCode::from(2);
        }
    };

    let Some(mut runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };

    match runtime.approve(approval_id, now).await {
        Ok(outcome) => print_json_or_fail(&outcome),
        Err(e) => {
            eprintln!("error: hyväksyntä epäonnistui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `status <task-id>`: prints the task's status.
async fn cmd_status(rest: &[String]) -> ExitCode {
    let Some(raw) = rest.first() else {
        eprintln!("error: status vaatii tehtävän tunnisteen (uuid)");
        return ExitCode::from(2);
    };
    let task_id = match raw.parse() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: kelvoton tehtävän tunniste: {e}");
            return ExitCode::from(2);
        }
    };

    let Some(runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };

    if let Some(status) = runtime.status(task_id).await {
        print_json_or_fail(&status)
    } else {
        eprintln!("error: tehtävää ei löydy: {task_id}");
        ExitCode::FAILURE
    }
}

/// `proof <task-id>`: prints the task's redacted proof bundle in `JSON` form.
fn cmd_proof(rest: &[String]) -> ExitCode {
    let Some(raw) = rest.first() else {
        eprintln!("error: proof vaatii tehtävän tunnisteen (uuid)");
        return ExitCode::from(2);
    };
    let task_id = match raw.parse() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: kelvoton tehtävän tunniste: {e}");
            return ExitCode::from(2);
        }
    };

    let Some(runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };

    if let Some(proof) = runtime.proof(task_id) {
        print_json_or_fail(proof)
    } else {
        eprintln!("error: todistetta ei löydy tehtävälle: {task_id}");
        ExitCode::FAILURE
    }
}

/// `pending`: prints the pending approvals (secret-free summaries).
fn cmd_pending() -> ExitCode {
    let Some(runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };
    let pending = runtime.pending_approvals();
    print_json_or_fail(&pending)
}

/// Resolves the skill id from the `submit-task` argument: either directly as
/// a `skill_id` UUID field, or via a `skill` name field (looked up in the list).
fn resolve_skill_id(runtime: &ActionRuntime, parsed: &Value) -> Result<SkillId, String> {
    if let Some(id_str) = parsed.get("skill_id").and_then(Value::as_str) {
        return id_str
            .parse()
            .map_err(|e| format!("kelvoton skill_id: {e}"));
    }
    if let Some(name) = parsed.get("skill").and_then(Value::as_str) {
        return runtime
            .list_skills()
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.id)
            .ok_or_else(|| format!("taitoa nimellä '{name}' ei löydy"));
    }
    Err("anna joko \"skill_id\" (uuid) tai \"skill\" (nimi)".to_string())
}

/// Serializes the value as pretty-printed `JSON` and writes it to standard
/// output. Returns a success exit code, or an error if serialization fails
/// (should not happen for public types).
fn print_json_or_fail<T: serde::Serialize>(value: &T) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: tulosteen sarjallistus epäonnistui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Prints usage instructions to stderr.
fn usage() {
    eprintln!(
        "familyclaw-actions-cli — operaattorin komentorivi (KERROS A)\n\
         \n\
         KÄYTTÖ:\n\
         \x20 actions list-skills\n\
         \x20 actions submit-task <json>\n\
         \x20 actions approve <approval-id>\n\
         \x20 actions status <task-id>\n\
         \x20 actions proof <task-id>\n\
         \x20 actions pending\n\
         \n\
         submit-task <json> esimerkki:\n\
         \x20 {{\"skill\":\"email_triage_mock\",\"payload\":{{\"emails\":[]}}}}"
    );
}
