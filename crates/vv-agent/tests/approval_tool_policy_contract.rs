use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentStatus, ApprovalBroker, ApprovalDecision, ApprovalError, ApprovalFuture,
    ApprovalPolicy, ApprovalProvider, ApprovalRequest, FunctionTool, LLMResponse, ModelRef,
    RunConfig, RunEventPayload, Runner, ScriptedModelProvider, ToolCall, ToolContext,
    ToolDirective, ToolOrchestrator, ToolOutput, ToolPolicy, ToolResultStatus, ToolRunOptions,
};

const CONTRACT_JSON: &str = include_str!("fixtures/parity/approval_tool_policy_v1.json");

#[derive(Debug, Deserialize)]
struct Contract {
    contract: String,
    request_id: RequestIdContract,
    tool_call: ToolCallContract,
    approval: ApprovalContract,
    policy: PolicyContract,
}

#[derive(Debug, Deserialize)]
struct RequestIdContract {
    regex: String,
    same_value_at: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ToolCallContract {
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct ApprovalContract {
    required_message: String,
    events: Vec<String>,
    result_shape: ResultShape,
    decisions: Vec<DecisionCase>,
    requested_event_metadata_keys: Vec<String>,
    resolved_event_metadata_keys: Vec<String>,
    provider_failure: ProviderFailureContract,
}

#[derive(Debug, Deserialize)]
struct DecisionCase {
    action: String,
    reason: String,
    metadata: Value,
    message: String,
    error_code: String,
}

#[derive(Debug, Deserialize)]
struct ProviderFailureContract {
    message: String,
    status: String,
    events: Vec<String>,
    tool_executes: bool,
    broker_retains_request: bool,
}

#[derive(Debug, Deserialize)]
struct PolicyContract {
    precedence: Vec<String>,
    message: String,
    error_code: String,
    result_shape: ResultShape,
    cases: Vec<PolicyCase>,
}

#[derive(Debug, Deserialize)]
struct PolicyCase {
    id: String,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    can_use_tool: bool,
    planned_tools: Vec<String>,
    policy_source: String,
}

#[derive(Debug, Deserialize)]
struct ResultShape {
    status: String,
    directive: String,
    content_keys: Vec<String>,
    metadata_keys: Vec<String>,
    mode: String,
}

#[derive(Default)]
struct ApprovalObservation {
    provider_request: Option<ApprovalRequest>,
    broker_pending: Option<ApprovalRequest>,
}

struct ContractApprovalProvider {
    broker: ApprovalBroker,
    decision: Option<ApprovalDecision>,
    observation: Arc<Mutex<ApprovalObservation>>,
}

impl ApprovalProvider for ContractApprovalProvider {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        self.observation
            .lock()
            .expect("approval observation")
            .provider_request = Some(request.clone());
        true
    }

    fn decide(&self, request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        self.observation
            .lock()
            .expect("approval observation")
            .broker_pending = self.broker.pending_request(&request.request_id);
        let decision = self.decision.clone();
        Box::pin(async move { Ok(decision) })
    }
}

struct FailingApprovalProvider {
    message: String,
    request_id: Arc<Mutex<String>>,
}

impl ApprovalProvider for FailingApprovalProvider {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        *self.request_id.lock().expect("request id lock") = request.request_id.clone();
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        let message = self.message.clone();
        Box::pin(async move { Err(ApprovalError::new(message)) })
    }
}

#[tokio::test]
async fn runner_approval_results_match_the_canonical_fixture() {
    let contract = contract();
    assert_eq!(contract.contract, "approval_tool_policy_v1");
    let request_id_pattern = Regex::new(&contract.request_id.regex).expect("request id regex");
    let mut request_ids = Vec::new();

    for decision_case in &contract.approval.decisions {
        let broker = ApprovalBroker::default();
        let observation = Arc::new(Mutex::new(ApprovalObservation::default()));
        let decision_metadata = decision_case
            .metadata
            .as_object()
            .expect("decision metadata object")
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let (decision, approval_timeout) = match decision_case.action.as_str() {
            "deny" => (
                Some(
                    ApprovalDecision::deny(decision_case.reason.clone())
                        .with_metadata(decision_metadata.clone()),
                ),
                None,
            ),
            "timeout" => (
                Some(
                    ApprovalDecision::timeout(decision_case.reason.clone())
                        .with_metadata(decision_metadata),
                ),
                None,
            ),
            action => panic!("unsupported fixture approval action: {action}"),
        };
        let provider = ContractApprovalProvider {
            broker: broker.clone(),
            decision,
            observation: observation.clone(),
        };
        let executions = Arc::new(AtomicUsize::new(0));
        let (runner, agent) = approval_runner(&contract, executions.clone());

        let mut run_config = RunConfig::builder()
            .approval_provider(Arc::new(provider))
            .approval_broker(broker);
        if let Some(timeout) = approval_timeout {
            run_config = run_config.approval_timeout(timeout);
        }
        let result = runner
            .run_with_config(&agent, "run approval contract", run_config.build())
            .await
            .expect("approval contract run");

        assert_eq!(result.status(), AgentStatus::Completed);
        assert_eq!(result.final_output(), Some("done"));
        assert_eq!(executions.load(Ordering::SeqCst), 0);

        let observed = observation.lock().expect("approval observation");
        let provider_request = observed
            .provider_request
            .as_ref()
            .expect("provider approval request");
        let broker_pending = observed
            .broker_pending
            .as_ref()
            .expect("broker pending approval");
        let tool_result = result
            .result()
            .cycles
            .iter()
            .flat_map(|cycle| &cycle.tool_results)
            .find(|tool_result| tool_result.tool_call_id == contract.tool_call.id)
            .expect("approval tool result");

        let mut event_names = Vec::new();
        let mut requested_id = None;
        let mut resolved_id = None;
        for event in result.events() {
            match event.payload() {
                RunEventPayload::ApprovalRequested {
                    request_id,
                    tool_call_id,
                    tool_name,
                    message,
                } => {
                    event_names.push("approval_requested".to_string());
                    assert_eq!(tool_call_id, &contract.tool_call.id);
                    assert_eq!(tool_name, &contract.tool_call.name);
                    assert_eq!(message, &contract.approval.required_message);
                    assert_eq!(
                        sorted_keys(event.metadata()),
                        sorted_values(contract.approval.requested_event_metadata_keys.clone())
                    );
                    assert_eq!(
                        event.metadata().get("arguments"),
                        Some(&contract.tool_call.arguments)
                    );
                    assert_eq!(
                        event.metadata().get("tool_name"),
                        Some(&Value::String(contract.tool_call.name.clone()))
                    );
                    requested_id = Some(request_id.clone());
                }
                RunEventPayload::ApprovalResolved {
                    request_id,
                    tool_call_id,
                    tool_name,
                    approved,
                } => {
                    event_names.push("approval_resolved".to_string());
                    assert_eq!(tool_call_id, &contract.tool_call.id);
                    assert_eq!(tool_name, &contract.tool_call.name);
                    assert!(!approved);
                    assert_eq!(
                        sorted_keys(event.metadata()),
                        sorted_values(contract.approval.resolved_event_metadata_keys.clone())
                    );
                    assert_eq!(
                        event.metadata().get("action"),
                        Some(&Value::String(decision_case.action.clone()))
                    );
                    assert_eq!(
                        event.metadata().get("reason"),
                        Some(&Value::String(decision_case.reason.clone()))
                    );
                    assert_eq!(
                        event.metadata().get("decision_metadata"),
                        Some(&decision_case.metadata)
                    );
                    resolved_id = Some(request_id.clone());
                }
                _ => {}
            }
        }
        assert_eq!(event_names, contract.approval.events);

        let requested_id = requested_id.expect("approval requested event");
        let resolved_id = resolved_id.expect("approval resolved event");
        let result_request_id = tool_result.metadata["request_id"]
            .as_str()
            .expect("tool result request id")
            .to_string();
        let values_by_path = BTreeMap::from([
            (
                "provider_request.request_id",
                provider_request.request_id.clone(),
            ),
            (
                "broker_pending.request_id",
                broker_pending.request_id.clone(),
            ),
            ("approval_requested_event.request_id", requested_id),
            ("approval_resolved_event.request_id", resolved_id),
            ("tool_result.metadata.request_id", result_request_id),
        ]);
        let canonical_request_id = &provider_request.request_id;
        assert!(request_id_pattern.is_match(canonical_request_id));
        for path in &contract.request_id.same_value_at {
            assert_eq!(
                values_by_path.get(path.as_str()),
                Some(canonical_request_id),
                "request id mismatch at {path}"
            );
        }
        request_ids.push(canonical_request_id.clone());

        assert_result_shape(tool_result, &contract.approval.result_shape);
        assert_eq!(
            tool_result.error_code.as_deref(),
            Some(decision_case.error_code.as_str())
        );
        assert_eq!(
            serde_json::from_str::<Value>(&tool_result.content).expect("approval result content"),
            json!({
                "ok": false,
                "error": decision_case.message,
                "error_code": decision_case.error_code,
                "tool_name": contract.tool_call.name,
            })
        );
        assert_eq!(
            serde_json::to_value(&tool_result.metadata).expect("approval result metadata"),
            json!({
                "mode": contract.approval.result_shape.mode,
                "request_id": canonical_request_id,
                "tool_name": contract.tool_call.name,
                "arguments": contract.tool_call.arguments,
                "action": decision_case.action,
                "message": decision_case.message,
            })
        );
    }

    request_ids.sort();
    request_ids.dedup();
    assert_eq!(request_ids.len(), contract.approval.decisions.len());
}

#[tokio::test]
async fn approval_provider_failure_matches_the_canonical_fixture() {
    let contract = contract();
    let failure = &contract.approval.provider_failure;
    let executions = Arc::new(AtomicUsize::new(0));
    let (runner, agent) = approval_runner(&contract, executions.clone());
    let broker = ApprovalBroker::default();
    let request_id = Arc::new(Mutex::new(String::new()));

    let result = runner
        .run_with_config(
            &agent,
            "run approval provider failure contract",
            RunConfig::builder()
                .approval_provider(Arc::new(FailingApprovalProvider {
                    message: failure.message.clone(),
                    request_id: request_id.clone(),
                }))
                .approval_broker(broker.clone())
                .build(),
        )
        .await
        .expect("provider failure should produce a failed run");

    assert_eq!(failure.status, "failed");
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.result().error.as_deref(),
        Some(failure.message.as_str())
    );
    assert_eq!(executions.load(Ordering::SeqCst) > 0, failure.tool_executes);
    let request_id = request_id.lock().expect("request id lock").clone();
    assert!(!request_id.is_empty());
    assert_eq!(
        broker.pending_request(&request_id).is_some(),
        failure.broker_retains_request
    );
    let lifecycle = result
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::ApprovalRequested { .. } => Some("approval_requested"),
            RunEventPayload::ApprovalResolved { .. } => Some("approval_resolved"),
            RunEventPayload::RunFailed { .. } => Some("run_failed"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle, failure.events);
}

#[tokio::test]
async fn orchestrator_policy_precedence_matches_the_canonical_fixture() {
    let contract = contract();
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder(contract.tool_call.name.clone())
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("unexpected execution"))
            }
        })
        .build()
        .expect("policy contract tool");
    let orchestrator = ToolOrchestrator::from_tools(vec![tool.to_executor()]);
    let mut observed_sources = Vec::new();

    for policy_case in &contract.policy.cases {
        let can_use_tool = policy_case.can_use_tool;
        let mut options = ToolRunOptions::default()
            .allow_only(policy_case.allowed_tools.clone())
            .planned_names(policy_case.planned_tools.clone())
            .can_use_tool(move |_tool_name, _arguments| can_use_tool);
        for tool_name in &policy_case.disallowed_tools {
            options = options.disallow(tool_name.clone());
        }
        let mut context = ToolContext::new("./workspace");
        let result = orchestrator
            .run_one(contract_tool_call(&contract), &mut context, options)
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", policy_case.id));

        assert_result_shape(&result, &contract.policy.result_shape);
        assert_eq!(
            result.error_code.as_deref(),
            Some(contract.policy.error_code.as_str())
        );
        assert_eq!(
            serde_json::from_str::<Value>(&result.content).expect("policy result content"),
            json!({
                "ok": false,
                "error": contract.policy.message,
                "error_code": contract.policy.error_code,
                "tool_name": contract.tool_call.name,
            }),
            "{}",
            policy_case.id
        );
        assert_eq!(
            serde_json::to_value(&result.metadata).expect("policy result metadata"),
            json!({
                "mode": contract.policy.result_shape.mode,
                "policy_source": policy_case.policy_source,
                "tool_name": contract.tool_call.name,
                "arguments": contract.tool_call.arguments,
                "message": contract.policy.message,
            }),
            "{}",
            policy_case.id
        );
        observed_sources.push(
            result.metadata["policy_source"]
                .as_str()
                .expect("policy source")
                .to_string(),
        );
    }

    assert_eq!(observed_sources, contract.policy.precedence);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

fn contract() -> Contract {
    serde_json::from_str(CONTRACT_JSON).expect("approval tool policy fixture")
}

fn approval_runner(contract: &Contract, executions: Arc<AtomicUsize>) -> (Runner, Agent) {
    let executions_for_tool = executions.clone();
    let dangerous = FunctionTool::builder(contract.tool_call.name.clone())
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("unexpected execution"))
            }
        })
        .build()
        .expect("approval contract tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-contract-model",
        vec![
            LLMResponse::with_tool_calls("", vec![contract_tool_call(contract)]),
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish-contract",
                    "task_finish",
                    json!({"message": "done"}),
                )],
            ),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("approval contract runner");
    let agent = Agent::builder("approval_contract_agent")
        .instructions("Exercise the approval contract.")
        .model(ModelRef::backend("scripted", "approval-contract-model"))
        .tool(dangerous)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .build()
        .expect("approval contract agent");
    (runner, agent)
}

fn contract_tool_call(contract: &Contract) -> ToolCall {
    ToolCall::from_raw_arguments(
        contract.tool_call.id.clone(),
        contract.tool_call.name.clone(),
        contract.tool_call.arguments.clone(),
    )
}

fn assert_result_shape(result: &vv_agent::ToolExecutionResult, shape: &ResultShape) {
    assert_eq!(shape.status, "error");
    assert_eq!(shape.directive, "continue");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.directive, ToolDirective::Continue);

    let content = serde_json::from_str::<Value>(&result.content).expect("JSON tool result");
    assert_exact_keys(&content, &shape.content_keys);
    let metadata = serde_json::to_value(&result.metadata).expect("tool result metadata");
    assert_exact_keys(&metadata, &shape.metadata_keys);
    assert_eq!(result.metadata["mode"], shape.mode);
}

fn sorted_keys(metadata: &BTreeMap<String, Value>) -> Vec<String> {
    metadata.keys().cloned().collect()
}

fn sorted_values(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

fn assert_exact_keys(value: &Value, expected: &[String]) {
    let mut actual = value
        .as_object()
        .expect("object value")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut expected = expected.to_vec();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}
