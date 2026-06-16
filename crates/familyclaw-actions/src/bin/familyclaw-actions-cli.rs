//! `familyclaw-actions-cli` — ohut operaattorin komentorivi toimintoajoympäristölle.
//!
//! Binääri on **pelkkä kuori** [`familyclaw_actions::ActionRuntime`]-julkisivun
//! päällä (KERROS A). Se ei sisällä omaa toimintologiikkaa eikä koske putken
//! sisäosiin — kaikki työ tehdään julkisivun kautta.
//!
//! ## Komennot
//! ```text
//! actions list-skills            rekisteröidyt taidot + riskiluokka (ei salaisuuksia)
//! actions submit-task <json>     lähetä tehtävä, tulosta tehtävän tunniste
//! actions approve <approval-id>  kuluta/merkitse hyväksyntä → jatka suoritus
//! actions status <task-id>       tulosta tehtävän tila
//! actions proof <task-id>        tulosta redaktoitu todistepaketti (JSON)
//! actions pending                tulosta odottavat hyväksynnät
//! ```
//!
//! `submit-task <json>` ottaa JSON-objektin muotoa
//! `{ "skill_id": "<uuid>", "payload": { ... } }` tai vaihtoehtoisesti
//! `{ "skill": "<name>", "payload": { ... } }`, jolloin taito haetaan nimellä.
//!
//! ## Turvallisuus
//! Tuloste ei koskaan sisällä salaisuuksia: taitolista näyttää vain julkiset
//! kentät, ja todistepaketti on jo redaktoitu putkessa. Binääri lukee kelloa
//! vain kerran I/O-rajalla (`now`) ja injektoi sen julkisivulle.

use std::process::ExitCode;

use serde_json::Value;

use familyclaw_actions::facade::ActionRuntime;
use familyclaw_actions::ids::{ApprovalId, SkillId};
use familyclaw_core::time::{now, Timestamp};

/// Komentorivin sisääntulo: jäsentää argumentit, ajaa komennon ja palauttaa
/// prosessin paluuarvon (`0` = onnistui, `1` = virhe, `2` = väärä käyttö).
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

/// Ajaa valitun alikomennon. Erotettu [`main`]-funktiosta, jotta async-logiikka
/// on testattavissa ja virheenkäsittely keskitetty.
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

/// Rakentaa ajoympäristön oletustaidoilla. Virhetilanteessa tulostaa selitteen.
fn build_runtime() -> Option<ActionRuntime> {
    match ActionRuntime::with_default_skills() {
        Ok(rt) => Some(rt),
        Err(e) => {
            eprintln!("error: ajoympäristön alustus epäonnistui: {e}");
            None
        }
    }
}

/// `list-skills`: tulostaa rekisteröidyt taidot (id, nimi, versio, riski,
/// hyväksyntävaatimus) yhtenä JSON-taulukkona. Ei salaisuuksia.
fn cmd_list_skills() -> ExitCode {
    let Some(runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };
    let skills = runtime.list_skills();
    print_json_or_fail(&skills)
}

/// `submit-task <json>`: lähettää tehtävän ja ajaa putken; tulostaa tehtävän
/// tunnisteen, tilan ja mahdollisen odottavan hyväksynnän tunnisteen.
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

/// `approve <approval-id>`: kuluttaa hyväksynnän ja ajaa tehtävän loppuun.
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

/// `status <task-id>`: tulostaa tehtävän tilan.
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

/// `proof <task-id>`: tulostaa tehtävän redaktoidun todistepaketin `JSON`-muodossa.
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

/// `pending`: tulostaa odottavat hyväksynnät (salaisuudettomat tiivistelmät).
fn cmd_pending() -> ExitCode {
    let Some(runtime) = build_runtime() else {
        return ExitCode::FAILURE;
    };
    let pending = runtime.pending_approvals();
    print_json_or_fail(&pending)
}

/// Ratkaisee taidon tunnisteen `submit-task`-argumentista: joko suorana
/// `skill_id`-UUID-kenttänä tai `skill`-nimikenttänä (haetaan luettelosta).
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

/// Sarjallistaa arvon kauniisti tulostettavaksi `JSON`-muotoon ja kirjoittaa sen
/// vakiotulosteeseen. Palauttaa onnistumisen paluuarvon, tai virheen jos
/// sarjallistus epäonnistuu (ei pitäisi tapahtua julkisilla tyypeillä).
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

/// Tulostaa käyttöohjeen virhevirtaan.
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
