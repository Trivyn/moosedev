//! Native response-provider compatibility probes for the v2 intent schemas.
use super::{association_schema, purpose_schema, AssociationChoice, PurposeChoice};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct IntentContractProbeUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentContractProbeAttempt {
    pub name: String,
    pub prompt: String,
    pub schema: Value,
    pub response: Option<Value>,
    pub usage: Option<IntentContractProbeUsage>,
    pub expected_decision: String,
    pub semantic_match: Option<bool>,
    pub semantic_error: Option<String>,
    pub passed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentContractProbeReceipt {
    pub passed: bool,
    pub response_receipt: crate::harness::response::ResponseReceipt,
    pub preparation_error: Option<String>,
    pub attempts: Vec<IntentContractProbeAttempt>,
}

#[derive(Clone, Copy)]
enum ProbeContract {
    Association,
    Purpose {
        handles: &'static [&'static str],
        has_next_page: bool,
        has_selected_purpose: bool,
    },
}

type ProbeCase = (
    &'static str,
    &'static str,
    Value,
    &'static str,
    ProbeContract,
);

fn probe_cases() -> [ProbeCase; 5] {
    [
        (
            "association_associate",
            "Deterministic response-format compatibility check. Neutral task: associate the proven format_export_timestamp entity with its governing record. Supplied records: r0 = accepted requirement that exported timestamps use UTC; r1 = accepted unrelated requirement that audio volume use decibels. Return this semantic shape with a short nonempty rationale: {\"decision\":\"associate\",\"rationale\":\"r0 governs the timestamp export entity.\",\"record_handles\":[\"r0\"]}",
            association_schema(&["r0".into(), "r1".into()]),
            "associate",
            ProbeContract::Association,
        ),
        (
            "association_none_applies",
            "Deterministic response-format compatibility check. Neutral task: assess a proven resize_thumbnail entity. Supplied records: r0 = requirement that exported timestamps use UTC; r1 = requirement that audio volume use decibels. Neither applies to image resizing. Return this semantic shape with a short nonempty rationale: {\"decision\":\"none_applies\",\"rationale\":\"Neither supplied record applies to image resizing.\"}",
            association_schema(&["r0".into(), "r1".into()]),
            "none_applies",
            ProbeContract::Association,
        ),
        (
            "purpose_select",
            "Deterministic response-format compatibility check. Neutral task: make exported timestamps use UTC. Supplied candidates: r0 = accepted requirement that exported timestamps use UTC; r1 = accepted unrelated requirement that audio volume use decibels. Select r0 as the task purpose. Return this semantic shape with a short nonempty rationale: {\"decision\":\"select\",\"rationale\":\"r0 states the UTC export behavior required by the task.\",\"record_handle\":\"r0\",\"role\":\"purpose\"}",
            purpose_schema(&["r0".into(), "r1".into()], false, false),
            "select",
            ProbeContract::Purpose { handles: &["r0", "r1"], has_next_page: false, has_selected_purpose: false },
        ),
        (
            "purpose_done",
            "Deterministic response-format compatibility check. Neutral task: make exported timestamps use UTC. The accepted UTC timestamp requirement r0 is already retained as the purpose, and the remaining audio-volume candidate is unrelated. Return this semantic shape with a short nonempty rationale: {\"decision\":\"done\",\"rationale\":\"The governing UTC timestamp purpose is already selected.\"}",
            purpose_schema(&["r1".into()], false, true),
            "done",
            ProbeContract::Purpose { handles: &["r1"], has_next_page: false, has_selected_purpose: true },
        ),
        (
            "purpose_missing_empty",
            "Deterministic response-format compatibility check. Neutral task: identify governing knowledge for a proposed checksum encoder, but bounded retrieval is exhausted and supplied candidates are empty. Return this semantic shape with a nonempty grounded reason: {\"decision\":\"missing\",\"grounded_reason\":\"Retrieval is exhausted without a supplied governing record for the checksum encoder.\"}",
            purpose_schema(&[], false, false),
            "missing",
            ProbeContract::Purpose { handles: &[], has_next_page: false, has_selected_purpose: false },
        ),
    ]
}

/// Exercise the actual intent schemas through the prepared native client.
/// This bounded compatibility probe has no task, filesystem, daemon, or graph side effects.
pub async fn probe_intent_contracts(
    config: &crate::llm::LlmConfig,
    policy: crate::harness::response::ResponsePolicy,
) -> Result<IntentContractProbeReceipt> {
    let prepared = match crate::harness::response::prepare(config, policy).await {
        Ok(prepared) => prepared,
        Err(error) => {
            return Ok(IntentContractProbeReceipt {
                passed: false,
                response_receipt: error.receipt,
                preparation_error: Some(
                    error
                        .cause
                        .to_string()
                        .replace(&config.api_key, "[redacted]"),
                ),
                attempts: vec![],
            });
        }
    };
    let cases = probe_cases();
    let mut attempts = Vec::with_capacity(cases.len());
    for (name, prompt, schema, expected, contract) in cases {
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            prepared.client.chat_completion_json_schema_checked(
                &config.model,
                prompt,
                None,
                name,
                schema.clone(),
            ),
        )
        .await;
        let (response, error, semantic_error) = match result {
            Err(_) => (
                None,
                Some(format!(
                    "intent contract probe `{name}` exceeded 60 seconds"
                )),
                None,
            ),
            Ok(Err(error)) => (
                None,
                Some(error.to_string().replace(&config.api_key, "[redacted]")),
                None,
            ),
            Ok(Ok(text)) => match serde_json::from_str::<Value>(&text) {
                Err(error) => (
                    None,
                    Some(format!(
                        "intent contract probe `{name}` returned invalid JSON: {error}"
                    )),
                    None,
                ),
                Ok(response) => {
                    let error = validate_contract_response(contract, &response)
                        .err()
                        .map(|error| error.to_string());
                    let semantic_error = if error.is_none() {
                        validate_intent_probe(expected, &response)
                            .err()
                            .map(|error| error.to_string())
                    } else {
                        None
                    };
                    (Some(response), error, semantic_error)
                }
            },
        };
        let usage =
            prepared
                .client
                .take_usage_observation()
                .map(
                    |(prompt_tokens, completion_tokens)| IntentContractProbeUsage {
                        prompt_tokens,
                        completion_tokens,
                    },
                );
        let passed = error.is_none();
        attempts.push(IntentContractProbeAttempt {
            name: name.into(),
            prompt: prompt.into(),
            schema,
            response,
            usage,
            expected_decision: expected.into(),
            semantic_match: error.is_none().then_some(semantic_error.is_none()),
            semantic_error,
            passed,
            error,
        });
    }
    Ok(IntentContractProbeReceipt {
        passed: attempts.iter().all(|attempt| attempt.passed),
        response_receipt: prepared.receipt,
        preparation_error: None,
        attempts,
    })
}

fn bounded_nonempty(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 2_000
}

/// Validate the controller's complete production contract after transport.
/// Providers validate the same schema when structured output is supported;
/// this also covers the ordinary-JSON fallback and controller-only uniqueness.
fn validate_contract_response(contract: ProbeContract, response: &Value) -> Result<()> {
    if matches!(contract, ProbeContract::Association) {
        match serde_json::from_value::<AssociationChoice>(response.clone())? {
            AssociationChoice::Associate {
                record_handles,
                rationale,
            } => {
                anyhow::ensure!(
                    bounded_nonempty(&rationale),
                    "association rationale is invalid"
                );
                anyhow::ensure!(
                    !record_handles.is_empty() && record_handles.len() <= 16,
                    "association handle count is invalid"
                );
                let mut seen = BTreeSet::new();
                for handle in record_handles {
                    anyhow::ensure!(
                        matches!(handle.as_str(), "r0" | "r1"),
                        "association handle is outside the supplied enum"
                    );
                    anyhow::ensure!(seen.insert(handle), "association handles are not distinct");
                }
            }
            AssociationChoice::NoneApplies { rationale } => {
                anyhow::ensure!(
                    bounded_nonempty(&rationale),
                    "association rationale is invalid"
                );
            }
        }
        return Ok(());
    }
    let ProbeContract::Purpose {
        handles,
        has_next_page,
        has_selected_purpose,
    } = contract
    else {
        unreachable!()
    };
    match serde_json::from_value::<PurposeChoice>(response.clone())? {
        PurposeChoice::Select {
            candidate_handle,
            role,
            rationale,
        } => {
            anyhow::ensure!(
                response.get("record_handle").and_then(Value::as_str)
                    == Some(candidate_handle.as_str())
                    && response.get("candidate_handle").is_none(),
                "purpose response does not match the strict record_handle schema"
            );
            anyhow::ensure!(
                handles.contains(&candidate_handle.as_str()),
                "purpose handle is outside the supplied enum"
            );
            anyhow::ensure!(
                matches!(role.as_str(), "purpose" | "obligation"),
                "purpose role is outside the supplied enum"
            );
            anyhow::ensure!(bounded_nonempty(&rationale), "purpose rationale is invalid");
        }
        PurposeChoice::Skip {
            candidate_handle,
            rationale,
        } => {
            anyhow::ensure!(
                response.get("record_handle").and_then(Value::as_str)
                    == Some(candidate_handle.as_str())
                    && response.get("candidate_handle").is_none(),
                "purpose response does not match the strict record_handle schema"
            );
            anyhow::ensure!(
                handles.contains(&candidate_handle.as_str()),
                "purpose handle is outside the supplied enum"
            );
            anyhow::ensure!(bounded_nonempty(&rationale), "purpose rationale is invalid");
        }
        PurposeChoice::NextPage { rationale } => {
            anyhow::ensure!(has_next_page, "next_page is unavailable without a cursor");
            anyhow::ensure!(bounded_nonempty(&rationale), "purpose rationale is invalid");
        }
        PurposeChoice::Done { rationale } => {
            anyhow::ensure!(
                has_selected_purpose,
                "done is unavailable without a selected purpose"
            );
            anyhow::ensure!(bounded_nonempty(&rationale), "purpose rationale is invalid");
        }
        PurposeChoice::Missing { grounded_reason } => {
            anyhow::ensure!(
                !has_next_page,
                "missing is unavailable while another candidate page remains"
            );
            anyhow::ensure!(
                bounded_nonempty(&grounded_reason),
                "purpose grounded reason is invalid"
            );
        }
    }
    Ok(())
}

fn validate_intent_probe(expected: &str, response: &Value) -> Result<()> {
    anyhow::ensure!(
        response["decision"].as_str() == Some(expected),
        "intent contract probe returned the wrong decision"
    );
    match expected {
        "associate" => {
            let parsed: AssociationChoice = serde_json::from_value(response.clone())?;
            let AssociationChoice::Associate {
                record_handles,
                rationale,
            } = parsed
            else {
                anyhow::bail!("intent associate probe returned a different variant")
            };
            anyhow::ensure!(
                !rationale.trim().is_empty(),
                "intent associate rationale is empty"
            );
            anyhow::ensure!(
                record_handles == ["r0"],
                "intent associate probe must select exactly r0"
            );
        }
        "none_applies" => {
            let parsed: AssociationChoice = serde_json::from_value(response.clone())?;
            let AssociationChoice::NoneApplies { rationale } = parsed else {
                anyhow::bail!("intent none probe returned a different variant")
            };
            anyhow::ensure!(
                !rationale.trim().is_empty(),
                "intent none rationale is empty"
            );
        }
        "select" => {
            let parsed: PurposeChoice = serde_json::from_value(response.clone())?;
            let PurposeChoice::Select {
                candidate_handle,
                role,
                rationale,
            } = parsed
            else {
                anyhow::bail!("purpose select probe returned a different variant")
            };
            anyhow::ensure!(
                candidate_handle == "r0" && role == "purpose" && !rationale.trim().is_empty(),
                "purpose select probe returned invalid semantics"
            );
        }
        "done" => {
            let parsed: PurposeChoice = serde_json::from_value(response.clone())?;
            let PurposeChoice::Done { rationale } = parsed else {
                anyhow::bail!("purpose done probe returned a different variant")
            };
            anyhow::ensure!(
                !rationale.trim().is_empty(),
                "purpose done rationale is empty"
            );
        }
        "missing" => {
            let parsed: PurposeChoice = serde_json::from_value(response.clone())?;
            let PurposeChoice::Missing { grounded_reason } = parsed else {
                anyhow::bail!("purpose missing probe returned a different variant")
            };
            anyhow::ensure!(
                !grounded_reason.trim().is_empty(),
                "purpose missing probe returned an empty grounded reason"
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_valid_different_branch_passes_contract_but_not_semantics() {
        let response = json!({"decision":"missing","grounded_reason":"A valid alternate branch."});
        assert!(validate_contract_response(
            ProbeContract::Purpose {
                handles: &["r0", "r1"],
                has_next_page: false,
                has_selected_purpose: false
            },
            &response
        )
        .is_ok());
        assert!(validate_intent_probe("select", &response).is_err());
    }

    #[test]
    fn out_of_schema_and_controller_invalid_responses_fail_contract() {
        assert!(validate_contract_response(
            ProbeContract::Purpose {
                handles: &["r0"],
                has_next_page: true,
                has_selected_purpose: false
            },
            &json!({"decision":"missing","grounded_reason":"More candidates remain."}),
        )
        .is_err());
        assert!(validate_contract_response(
            ProbeContract::Purpose { handles: &["r0", "r1"], has_next_page: false, has_selected_purpose: false },
            &json!({"decision":"select","record_handle":"r9","role":"purpose","rationale":"Unknown handle."}),
        )
        .is_err());
        assert!(validate_contract_response(
            ProbeContract::Purpose { handles: &["r0", "r1"], has_next_page: false, has_selected_purpose: false },
            &json!({"decision":"select","candidate_handle":"r0","role":"purpose","rationale":"Legacy fallback spelling."}),
        )
        .is_err());
        assert!(validate_contract_response(
            ProbeContract::Association,
            &json!({"decision":"associate","record_handles":["r0","r0"],"rationale":"Duplicate handles."}),
        )
        .is_err());
        assert!(validate_contract_response(
            ProbeContract::Association,
            &json!({"decision":"associate","record_handles":[],"rationale":"Empty selection."}),
        )
        .is_err());
    }

    #[test]
    fn prompt_examples_follow_actual_schema_property_order() {
        for (_, prompt, schema, expected, contract) in probe_cases() {
            let raw = prompt.rsplit_once(": ").unwrap().1;
            let example: Value = serde_json::from_str(raw).unwrap();
            assert_eq!(raw, serde_json::to_string(&example).unwrap());
            let branch = schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|branch| branch["properties"]["decision"]["const"] == expected)
                .unwrap();
            assert_eq!(
                example.as_object().unwrap().keys().collect::<Vec<_>>(),
                branch["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>()
            );
            validate_contract_response(contract, &example).unwrap();
        }
    }

    #[test]
    fn exact_response_passes_contract_and_semantics() {
        let response = json!({"decision":"select","record_handle":"r0","role":"purpose","rationale":"UTC export is the task purpose."});
        assert!(validate_contract_response(
            ProbeContract::Purpose {
                handles: &["r0", "r1"],
                has_next_page: false,
                has_selected_purpose: false
            },
            &response
        )
        .is_ok());
        assert!(validate_intent_probe("select", &response).is_ok());
    }
}
