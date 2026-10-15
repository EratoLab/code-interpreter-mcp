//! Bounded application metrics. Scrapes only read the recorder, never worker state.
use crate::config::AppConfig;
use anyhow::{Context, Result};
use metrics::{
    Unit, counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram,
};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use std::time::Instant;

pub const CALLS: &str = "code_interpreter_mcp_tool_calls_total";
pub const CALL_DURATION: &str = "code_interpreter_mcp_tool_call_duration_seconds";
pub const DEPTH: &str = "code_interpreter_mcp_execution_queue_depth";
pub const CAPACITY: &str = "code_interpreter_mcp_execution_queue_capacity";
pub const WAIT: &str = "code_interpreter_mcp_execution_queue_wait_seconds";
pub const WORKERS: &str = "code_interpreter_mcp_workers";
pub const EXECUTION: &str = "code_interpreter_mcp_python_execution_duration_seconds";
pub const INIT: &str = "code_interpreter_mcp_isolate_initialization_duration_seconds";
pub const REJECTIONS: &str = "code_interpreter_mcp_execution_rejections_total";
pub const EXITS: &str = "code_interpreter_mcp_worker_unexpected_exits_total";
pub const OUTCOMES: &[&str] = &[
    "success",
    "validation_error",
    "overload",
    "python_error",
    "timeout",
    "cancelled",
    "runtime_failure",
];
pub const BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1., 2.5, 5., 10., 30., 60., 120., 300., 600.,
];
pub const EXECUTION_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1., 2.5, 5., 10., 30., 60., 120.,
];

pub fn builder() -> Result<PrometheusBuilder> {
    let mut builder = PrometheusBuilder::new();
    for name in [CALL_DURATION, WAIT, INIT, EXECUTION] {
        builder = builder.set_buckets_for_metric(
            Matcher::Full(name.into()),
            if name == EXECUTION {
                EXECUTION_BUCKETS
            } else {
                BUCKETS
            },
        )?;
    }
    Ok(builder)
}

/// Owned by application startup so listener/reporting tasks stop on shutdown or startup failure.
pub struct MetricsTasks(Vec<tokio::task::JoinHandle<()>>);
impl Drop for MetricsTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

pub async fn init(config: &AppConfig) -> Result<Option<MetricsTasks>> {
    let Some(address) = config
        .integrations
        .prometheus
        .listen_address(&config.bind_addr)?
    else {
        return Ok(None);
    };
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind Prometheus listener to {address}"))?;
    let recorder = builder()?.build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder)
        .context("failed to install Prometheus recorder: a global recorder is already installed")?;
    register(config.workers.queue_capacity);
    let upkeep_handle = handle.clone();
    let router = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let handle = handle.clone();
            async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; version=0.0.4; charset=utf-8",
                    )],
                    handle.render(),
                )
            }
        }),
    );
    let exporter = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            eprintln!("Prometheus listener failed: {error}");
        }
    });
    let reporter = tokio::spawn(report_tokio(upkeep_handle));
    eprintln!("Prometheus metrics enabled at http://{address}/metrics (not a readiness probe)");
    Ok(Some(MetricsTasks(vec![exporter, reporter])))
}

// The built-in metrics-rs reporter converts durations to integer microseconds.
// Use the stable monitor directly to preserve fractional seconds consistently.
async fn report_tokio(handle: metrics_exporter_prometheus::PrometheusHandle) {
    let monitor = tokio_metrics::RuntimeMonitor::new(&tokio::runtime::Handle::current());
    for (name, description) in [
        ("tokio_workers_count", "Main runtime worker threads"),
        ("tokio_live_tasks_count", "Main runtime live tasks"),
        (
            "tokio_global_queue_depth",
            "Main runtime global queue depth",
        ),
        (
            "tokio_total_park_count",
            "Worker parks during the reporting interval",
        ),
        (
            "tokio_min_park_count",
            "Minimum worker parks during the reporting interval",
        ),
        (
            "tokio_max_park_count",
            "Maximum worker parks during the reporting interval",
        ),
    ] {
        describe_gauge!(name, Unit::Count, description);
    }
    for (name, description) in [
        (
            "tokio_total_busy_duration_seconds",
            "Sum of worker busy time during the reporting interval",
        ),
        (
            "tokio_min_busy_duration_seconds",
            "Minimum worker busy time during the reporting interval",
        ),
        (
            "tokio_max_busy_duration_seconds",
            "Maximum worker busy time during the reporting interval",
        ),
    ] {
        describe_gauge!(name, Unit::Seconds, description);
    }
    describe_gauge!(
        "tokio_busy_ratio",
        "Fraction of main runtime worker time spent busy"
    );
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    for snapshot in monitor.intervals() {
        handle.run_upkeep();
        gauge!("tokio_workers_count").set(snapshot.workers_count as f64);
        gauge!("tokio_live_tasks_count").set(snapshot.live_tasks_count as f64);
        gauge!("tokio_global_queue_depth").set(snapshot.global_queue_depth as f64);
        gauge!("tokio_total_park_count").set(snapshot.total_park_count as f64);
        gauge!("tokio_min_park_count").set(snapshot.min_park_count as f64);
        gauge!("tokio_max_park_count").set(snapshot.max_park_count as f64);
        gauge!("tokio_total_busy_duration_seconds").set(snapshot.total_busy_duration.as_secs_f64());
        gauge!("tokio_min_busy_duration_seconds").set(snapshot.min_busy_duration.as_secs_f64());
        gauge!("tokio_max_busy_duration_seconds").set(snapshot.max_busy_duration.as_secs_f64());
        let available = snapshot.elapsed.as_secs_f64() * snapshot.workers_count as f64;
        gauge!("tokio_busy_ratio").set(if available > 0. {
            snapshot.total_busy_duration.as_secs_f64() / available
        } else {
            0.
        });
        interval.tick().await;
    }
}

pub fn register(capacity: usize) {
    describe_counter!(
        CALLS,
        Unit::Count,
        "Completed tool calls by tool and outcome"
    );
    describe_counter!(
        REJECTIONS,
        Unit::Count,
        "Execution requests refused by reason"
    );
    describe_counter!(
        EXITS,
        Unit::Count,
        "Unexpected worker exits reducing capacity"
    );
    describe_gauge!(DEPTH, Unit::Count, "Jobs waiting for a worker");
    describe_gauge!(CAPACITY, Unit::Count, "Configured waiting queue capacity");
    describe_gauge!(
        WORKERS,
        Unit::Count,
        "Live workers by mutually exclusive state"
    );
    for (name, description) in [
        (
            CALL_DURATION,
            "Tool handler time including queue and initialization",
        ),
        (WAIT, "Time queued until dispatch or removal"),
        (INIT, "Isolate initialization time by phase and outcome"),
        (
            EXECUTION,
            "Python execution time excluding queue and initialization",
        ),
    ] {
        describe_histogram!(name, Unit::Seconds, description);
    }
    for tool in ["run_python_code", "list_code_snippets", "get_code_snippet"] {
        for &outcome in OUTCOMES {
            counter!(CALLS, "tool" => tool, "outcome" => outcome).absolute(0);
            let _ = histogram!(CALL_DURATION, "tool" => tool, "outcome" => outcome);
        }
    }
    for reason in ["validation", "queue_full", "shutdown"] {
        counter!(REJECTIONS, "reason" => reason).absolute(0);
    }
    for outcome in ["dispatched", "cancelled", "shutdown"] {
        let _ = histogram!(WAIT, "outcome" => outcome);
    }
    for outcome in ["success", "python_error", "timeout", "runtime_failure"] {
        let _ = histogram!(EXECUTION, "outcome" => outcome);
    }
    for phase in ["startup", "on_demand", "replenishment"] {
        for outcome in ["success", "runtime_failure"] {
            let _ = histogram!(INIT, "phase" => phase, "outcome" => outcome);
        }
    }
    counter!(EXITS).absolute(0);
    gauge!(DEPTH).set(0.);
    gauge!(CAPACITY).set(capacity as f64);
    for state in ["idle", "initializing", "executing"] {
        gauge!(WORKERS, "state" => state).set(0.);
    }
}

pub fn rejection(reason: &'static str) {
    counter!(REJECTIONS, "reason" => reason).increment(1);
}
pub fn queue_removed(start: Instant, outcome: &'static str) {
    gauge!(DEPTH).decrement(1.);
    histogram!(WAIT, "outcome" => outcome).record(start.elapsed().as_secs_f64());
}

/// Default cancellation also covers dropped handler futures; explicit completion counts once.
pub struct ToolCall {
    start: Instant,
    tool: &'static str,
    outcome: &'static str,
}
impl ToolCall {
    pub fn new(tool: &'static str) -> Self {
        Self {
            start: Instant::now(),
            tool,
            outcome: "cancelled",
        }
    }
    pub fn finish(&mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}
impl Drop for ToolCall {
    fn drop(&mut self) {
        counter!(CALLS, "tool" => self.tool, "outcome" => self.outcome).increment(1);
        histogram!(CALL_DURATION, "tool" => self.tool, "outcome" => self.outcome)
            .record(self.start.elapsed().as_secs_f64());
    }
}

pub struct WorkerState(&'static str);
impl WorkerState {
    pub fn new() -> Self {
        gauge!(WORKERS, "state" => "idle").increment(1.);
        Self("idle")
    }
    pub fn set(&mut self, state: &'static str) {
        gauge!(WORKERS, "state" => self.0).decrement(1.);
        gauge!(WORKERS, "state" => state).increment(1.);
        self.0 = state;
    }
}
impl Default for WorkerState {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for WorkerState {
    fn drop(&mut self) {
        gauge!(WORKERS, "state" => self.0).decrement(1.);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_once_preserves_fractional_seconds_and_balances_worker_states() {
        let recorder = builder().unwrap().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            register(8);
            let mut call = ToolCall::new("run_python_code");
            call.finish("success");
            drop(call);
            drop(ToolCall::new("run_python_code"));
            let mut worker = WorkerState::new();
            worker.set("initializing");
            worker.set("executing");
            drop(worker);
            histogram!(EXECUTION, "outcome" => "success").record(0.025);
        });
        let output = handle.render();
        assert!(output.contains("# TYPE code_interpreter_mcp_tool_calls_total counter"));
        assert!(output.contains("# HELP code_interpreter_mcp_execution_queue_depth"));
        for outcome in ["success", "cancelled"] {
            assert!(output.contains(&format!("code_interpreter_mcp_tool_calls_total{{tool=\"run_python_code\",outcome=\"{outcome}\"}} 1")), "{output}");
        }
        assert!(output.contains(
            "code_interpreter_mcp_python_execution_duration_seconds_sum{outcome=\"success\"} 0.025"
        ));
        for state in ["idle", "initializing", "executing"] {
            assert!(output.contains(&format!(
                "code_interpreter_mcp_workers{{state=\"{state}\"}} 0"
            )));
        }
    }

    #[tokio::test]
    async fn disabled_and_bind_failure_do_not_install_recorder() {
        let mut config: AppConfig = toml::from_str("").unwrap();
        assert!(init(&config).await.unwrap().is_none());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        config.integrations.prometheus.enabled = true;
        config.integrations.prometheus.port = listener.local_addr().unwrap().port().into();
        let error = init(&config).await.err().unwrap().to_string();
        assert!(
            error.contains("failed to bind Prometheus listener"),
            "{error}"
        );
    }
}
