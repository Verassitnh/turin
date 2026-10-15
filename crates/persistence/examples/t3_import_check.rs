//! Check a T3 `statev2.sqlite` against Turin's migrations.
//!
//! Prints counts and schema facts only. The source is opened read-only. The
//! snapshot is deleted when this process exits. Nothing is written under the
//! Turin data directory.

use std::path::PathBuf;
use std::process::ExitCode;

use turin_persistence::{ImportReport, T3Import};

fn main() -> ExitCode {
    let src = match std::env::args().nth(1) {
        Some(path) => PathBuf::from(path),
        None => default_t3_database(),
    };
    let snapshot = match T3Import::snapshot(&src) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            eprintln!("t3 import check failed: {err}");
            return ExitCode::from(1);
        }
    };
    match T3Import::migrate_and_verify(snapshot.path()) {
        Ok(report) => {
            print_report(&report);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("t3 import check failed: {err}");
            ExitCode::from(1)
        }
    }
}

fn default_t3_database() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".t3/userdata/statev2.sqlite")
}

fn print_report(report: &ImportReport) {
    println!("ledger_latest={}", report.plan.latest_id);
    println!("ledger_recorded={}", report.plan.recorded.len());
    println!("pending={}", report.plan.pending.len());
    println!("applied={}", report.applied.len());
    println!("tables={}", report.tables);
    println!("projects={}", report.projects);
    println!("active_projects={}", report.active_projects);
    println!("threads={}", report.threads);
    println!("active_threads={}", report.active_threads);
    println!("v2_threads={}", report.v2_threads);
    println!("active_v2_threads={}", report.active_v2_threads);
    println!("events_decoded={}", report.events.decoded);
    println!("events_unknown={}", report.events.unknown);
    println!("agent_decoded={}", report.events.agent_decoded);
    println!("agent_unknown={}", report.events.agent_unknown);
    println!("legacy_thread_rows={}", report.events.legacy_thread_rows);
    println!("rows:");
    for (name, count) in &report.row_counts {
        println!("  {name}={count}");
    }
    println!("event_types:");
    for (name, count) in &report.events.by_type {
        println!("  {name}={count}");
    }
    if !report.events.unknown_by_type.is_empty() {
        println!("unknown_event_types:");
        for (name, count) in &report.events.unknown_by_type {
            println!("  {name}={count}");
        }
    }
    if !report.events.legacy_by_type.is_empty() {
        println!("legacy_event_types:");
        for (name, count) in &report.events.legacy_by_type {
            println!("  {name}={count}");
        }
    }
}
