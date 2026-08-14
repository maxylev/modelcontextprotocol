//! Sanitized metrics collection and the final acceptance summary.
//!
//! Nothing printed here ever contains the API key, authorization headers, or
//! raw OpenRouter response bodies: only case metadata, bounded fixture
//! content, and aggregate counters.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::harness::Usage;

#[derive(Debug, Clone, Default)]
pub struct CaseMetric {
    pub case_id: &'static str,
    pub server: &'static str,
    pub tool: &'static str,
    pub status: &'static str, // ok | deviation | error
    pub request_ids: Vec<String>,
    pub actual_models: Vec<String>,
    pub usage: Usage,
    pub elapsed_ms: u64,
    pub mcp_ok: bool,
    pub roundtrip: bool,
    pub retries: u32,
    pub note: String,
}

#[derive(Debug, Default)]
pub struct Counters {
    pub requests: AtomicU64,
    pub tool_calls: AtomicU64,
    pub full_roundtrips: AtomicU64,
    pub consumption_requests: AtomicU64,
    pub retries: AtomicU32,
    /// Retries of a whole roundtrip after a model-echo deviation (the
    /// argument guard still ran and blocked before any MCP execution on
    /// every attempt). Oracle and server failures are never retried.
    pub echo_retries: AtomicU32,
    pub tokens_in: AtomicU64,
    pub tokens_out: AtomicU64,
}

#[derive(Debug, Default)]
pub struct Metrics {
    pub cases: Mutex<Vec<CaseMetric>>,
    pub counters: Counters,
    pub diagnostics: Mutex<Vec<String>>,
    /// Sanitized failure descriptions; loss-proof (owned by the shared
    /// metrics) so a budget abort cannot swallow them.
    pub failures: Mutex<Vec<String>>,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn record(&self, metric: CaseMetric) {
        self.cases.lock().expect("metrics lock").push(metric);
    }
    pub fn diagnostic(&self, message: String) {
        self.diagnostics.lock().expect("metrics lock").push(message);
    }
    pub fn failure(&self, message: String) {
        self.failures.lock().expect("metrics lock").push(message);
    }
    pub fn take_failures(&self) -> Vec<String> {
        std::mem::take(&mut self.failures.lock().expect("metrics lock"))
    }
}

fn fmt_usages(usage: &Usage) -> String {
    let parts: Vec<String> = [
        usage.prompt_tokens.map(|t| format!("in {t}")),
        usage.completion_tokens.map(|t| format!("out {t}")),
        usage.total_tokens.map(|t| format!("total {t}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "usage n/a".to_string()
    } else {
        parts.join(", ")
    }
}

/// Print the per-case table and the aggregate summary. Sanitized by
/// construction: request IDs and model names are not secrets, and no
/// response bodies or headers are included.
pub fn print_summary(metrics: &Metrics, requested_model: &str) {
    println!();
    println!("================================================================");
    println!("OpenRouter e2e acceptance summary");
    println!("================================================================");
    println!("requested model: {requested_model}");

    let cases = metrics.cases.lock().expect("metrics lock");
    if cases.is_empty() {
        println!("(no cases executed)");
    } else {
        println!();
        println!(
            "{:<10} {:<10} {:<22} {:<10} {:<6} {:<8} {:<8} {:<18}",
            "case", "server", "tool", "status", "mcp", "round", "retry", "elapsed"
        );
        for case in cases.iter() {
            let models = case
                .actual_models
                .first()
                .cloned()
                .unwrap_or_else(|| "-".to_string());
            let elapsed = case.elapsed_ms as f64 / 1000.0;
            println!(
                "{:<10} {:<10} {:<22} {:<10} {:<6} {:<8} {:<8} {elapsed:.1}s",
                case.case_id,
                case.server,
                case.tool,
                case.status,
                if case.mcp_ok { "ok" } else { "ERR" },
                if case.roundtrip { "yes" } else { "no" },
                case.retries,
            );
            println!("    note: {}", case.note);
            if !models.is_empty() && models != "-" {
                println!(
                    "    actual model: {models} | request ids: {} | {}",
                    case.request_ids.join(", "),
                    fmt_usages(&case.usage)
                );
            }
        }
    }
    drop(cases);

    let counters = &metrics.counters;
    println!();
    println!("aggregate:");
    println!(
        "  real OpenRouter HTTP attempts : {}",
        counters.requests.load(Ordering::SeqCst)
    );
    println!(
        "  MCP tool calls           : {}",
        counters.tool_calls.load(Ordering::SeqCst)
    );
    println!(
        "  full roundtrips          : {}",
        counters.full_roundtrips.load(Ordering::SeqCst)
    );
    println!(
        "  prompt/resource requests : {}",
        counters.consumption_requests.load(Ordering::SeqCst)
    );
    println!(
        "  retries                  : {}",
        counters.retries.load(Ordering::SeqCst)
    );
    println!(
        "  model-echo deviation retries : {}",
        counters.echo_retries.load(Ordering::SeqCst)
    );
    println!(
        "  tokens (in/out)          : {} / {}",
        counters.tokens_in.load(Ordering::SeqCst),
        counters.tokens_out.load(Ordering::SeqCst)
    );

    let diagnostics = metrics.diagnostics.lock().expect("metrics lock");
    if !diagnostics.is_empty() {
        println!();
        println!("normalizer/schema diagnostics:");
        for line in diagnostics.iter() {
            println!("  - {line}");
        }
    }
    println!("================================================================");
}

/// Panic with a sanitized aggregate of the failures so the acceptance test
/// fails loudly, listing every failing case without any secrets.
pub fn panic_on_failures(metrics: &Metrics) {
    let failures = metrics.take_failures();
    if failures.is_empty() {
        return;
    }
    let mut message = format!(
        "{} failure(s) reported ({} cases recorded):\n",
        failures.len(),
        metrics.cases.lock().expect("metrics lock").len()
    );
    for failure in failures {
        let bounded: String = failure.chars().take(600).collect();
        message.push_str(&format!("  - {bounded}\n"));
    }
    panic!("{message}");
}
