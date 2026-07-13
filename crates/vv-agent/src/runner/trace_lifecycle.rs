use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::tracing::{Span, TraceSink};

pub(super) struct RunTrace {
    state: Arc<Mutex<RunTraceState>>,
}

#[derive(Clone)]
pub(super) struct RunTraceObserver {
    state: Arc<Mutex<RunTraceState>>,
}

struct RunTraceState {
    sink: Option<Arc<dyn TraceSink>>,
    run_span: Span,
    agent_span: Span,
    tool_spans: BTreeMap<String, Span>,
    ended_run_span: Option<Span>,
    finished: bool,
}

impl RunTrace {
    pub(super) fn start(
        sink: Option<Arc<dyn TraceSink>>,
        trace_id: &str,
        run_id: &str,
        agent_name: &str,
        workflow_name: Option<&str>,
    ) -> Self {
        let run_span = Span::new(trace_id, "run")
            .with_metadata("run_id", Value::String(run_id.to_string()))
            .with_metadata("agent_name", Value::String(agent_name.to_string()))
            .with_metadata(
                "workflow_name",
                workflow_name
                    .map(|name| Value::String(name.to_string()))
                    .unwrap_or(Value::Null),
            );
        let agent_span = Span::new(trace_id, "agent")
            .with_parent_id(run_span.span_id.clone())
            .with_metadata("run_id", Value::String(run_id.to_string()))
            .with_metadata("agent_name", Value::String(agent_name.to_string()));
        if let Some(sink) = sink.as_ref() {
            notify_sink("on_span_start", || sink.on_span_start(&run_span));
            notify_sink("on_span_start", || sink.on_span_start(&agent_span));
        }
        Self {
            state: Arc::new(Mutex::new(RunTraceState {
                sink,
                run_span,
                agent_span,
                tool_spans: BTreeMap::new(),
                ended_run_span: None,
                finished: false,
            })),
        }
    }

    pub(super) fn observer(&self) -> RunTraceObserver {
        RunTraceObserver {
            state: self.state.clone(),
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sink
            .is_some()
    }

    pub(super) fn finish(&self, status: &str, detail: Option<(&str, Value)>) -> Span {
        finish_state(&self.state, status, detail)
    }
}

impl Drop for RunTrace {
    fn drop(&mut self) {
        let _ = finish_state(
            &self.state,
            "failed",
            Some(("error", Value::String("run aborted".to_string()))),
        );
    }
}

impl RunTraceObserver {
    pub(super) fn on_event(&self, event: &str, payload: &BTreeMap<String, Value>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finished {
            return;
        }
        match event {
            "tool_call_started" => {
                let Some(tool_call_id) = payload
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                else {
                    return;
                };
                if let Some(previous) = state.tool_spans.remove(tool_call_id) {
                    end_span(
                        state.sink.as_ref(),
                        previous.with_metadata("status", Value::String("replaced".to_string())),
                    );
                }
                let span = Span::new(&state.run_span.trace_id, "tool")
                    .with_parent_id(state.agent_span.span_id.clone())
                    .with_metadata(
                        "tool_name",
                        Value::String(
                            payload
                                .get("tool_name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        ),
                    )
                    .with_metadata(
                        "agent_name",
                        state
                            .agent_span
                            .metadata
                            .get("agent_name")
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                if let Some(sink) = state.sink.as_ref() {
                    notify_sink("on_span_start", || sink.on_span_start(&span));
                }
                state.tool_spans.insert(tool_call_id.to_string(), span);
            }
            "tool_result" | "tool_call_completed" => {
                let Some(tool_call_id) = payload.get("tool_call_id").and_then(Value::as_str) else {
                    return;
                };
                if let Some(span) = state.tool_spans.remove(tool_call_id) {
                    let span = if let Some(status) = payload.get("status").cloned() {
                        span.with_metadata("status", status)
                    } else {
                        span
                    };
                    end_span(state.sink.as_ref(), span);
                }
            }
            _ => {}
        }
    }
}

fn finish_state(
    state: &Arc<Mutex<RunTraceState>>,
    status: &str,
    detail: Option<(&str, Value)>,
) -> Span {
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.finished {
        return state
            .ended_run_span
            .clone()
            .unwrap_or_else(|| state.run_span.clone().finish());
    }
    state.finished = true;
    let sink = state.sink.clone();
    let tool_spans = std::mem::take(&mut state.tool_spans);
    for (_, span) in tool_spans.into_iter().rev() {
        end_span(
            sink.as_ref(),
            span.with_metadata("status", Value::String("abandoned".to_string())),
        );
    }
    let mut agent_span = state
        .agent_span
        .clone()
        .with_metadata("status", Value::String(status.to_string()));
    let mut run_span = state
        .run_span
        .clone()
        .with_metadata("status", Value::String(status.to_string()));
    if let Some((key, value)) = detail {
        agent_span = agent_span.with_metadata(key, value.clone());
        run_span = run_span.with_metadata(key, value);
    }
    end_span(sink.as_ref(), agent_span);
    let ended_run_span = end_span(sink.as_ref(), run_span);
    state.ended_run_span = Some(ended_run_span.clone());
    if let Some(sink) = sink.as_ref() {
        match catch_unwind(AssertUnwindSafe(|| sink.flush())) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("warning: trace sink flush failed: {error}"),
            Err(_) => eprintln!("warning: trace sink flush panicked"),
        }
    }
    ended_run_span
}

fn end_span(sink: Option<&Arc<dyn TraceSink>>, span: Span) -> Span {
    let ended = span.finish();
    if let Some(sink) = sink {
        notify_sink("on_span_end", || sink.on_span_end(&ended));
    }
    ended
}

fn notify_sink(operation: &str, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        eprintln!("warning: trace sink {operation} panicked");
    }
}
