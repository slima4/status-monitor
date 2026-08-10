//! Human-in-the-loop confirmation for write tools, via MCP elicitation.
//!
//! Before any mutation the tool calls [`require_confirmation`], which asks the
//! client (out of band of the tool arguments) to confirm. The write proceeds
//! only on an explicit `confirm = true`. Everything else — the user declines or
//! cancels, the client doesn't support elicitation, the response doesn't parse
//! — **fails closed**: no write fires. This is what stops a prompt-injected
//! "pause all my monitors" from executing without the human agreeing.
//!
//! Each refusal carries its own code, so an audit row starting `not_confirmed`
//! means a human decided — a client that cannot ask, and one that broke while
//! asking, need different fixes and neither is the caller's.

use rmcp::RoleServer;
use rmcp::service::{ElicitationError, ElicitationMode, RequestContext, ServiceError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::{McpToolError, codes};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Confirmation {
    /// Set to true to approve this action. Anything else cancels it.
    confirm: bool,
}

rmcp::elicit_safe!(Confirmation);

/// True when the connected client negotiated elicitation at `initialize`, so a
/// write tool can ask for confirmation. A peer that never initialized (a bare
/// `tools/list` probe) reports `None`, which is unknown, not unsupported — the
/// elicit call itself is the fail-closed backstop there.
pub fn client_can_confirm(ctx: &RequestContext<RoleServer>) -> bool {
    // Mirror the gate `elicit` itself applies: a url-only client declares the
    // capability but cannot answer a form, and must not be offered write tools.
    ctx.peer.peer_info().is_none()
        || ctx
            .peer
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
}

fn unsupported() -> McpToolError {
    McpToolError::new(
        codes::ELICITATION_UNSUPPORTED,
        "this MCP client cannot prompt for confirmation, and no change is made \
         without one; run the connector from a client that supports elicitation, \
         or make this change in the Uptimepage app",
        false,
    )
}

fn declined(detail: &'static str) -> McpToolError {
    McpToolError::new(
        codes::NOT_CONFIRMED,
        "the action was not confirmed; no change was made",
        false,
    )
    .with_detail(detail)
}

fn classify(err: &ElicitationError) -> McpToolError {
    match err {
        ElicitationError::UserDeclined => declined("declined"),
        ElicitationError::UserCancelled => declined("cancelled"),
        // Negotiated at `initialize` but no form mode — still the client's gap.
        ElicitationError::CapabilityNotSupported => unsupported(),
        _ => McpToolError::new(
            codes::CONFIRMATION_FAILED,
            "this MCP client did not answer the confirmation prompt; no change was made",
            false,
        )
        .with_detail(match err {
            ElicitationError::ParseError { .. } => "parse_error",
            ElicitationError::NoContent => "no_content",
            ElicitationError::Service(ServiceError::Timeout { .. }) => "timed_out",
            ElicitationError::Service(_) => "service_error",
            _ => "unknown",
        }),
    }
}

/// Ask the user to approve `message`. `Ok(())` only on an explicit
/// `confirm = true`; every other outcome fails closed.
pub async fn require_confirmation(
    ctx: &RequestContext<RoleServer>,
    message: impl Into<String>,
) -> Result<(), McpToolError> {
    if !client_can_confirm(ctx) {
        return Err(unsupported());
    }
    match ctx.peer.elicit::<Confirmation>(message.into()).await {
        Ok(Some(c)) if c.confirm => Ok(()),
        // Accepted the form with the box left false: a decision, not a fault.
        Ok(_) => Err(declined("unchecked")),
        // The error renders the client's raw form response, so only the
        // classification is logged.
        Err(e) => Err(classify(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_err() -> ElicitationError {
        ElicitationError::ParseError {
            error: serde_json::from_str::<u32>("\"x\"").unwrap_err(),
            data: serde_json::json!({"confirm": "x"}),
        }
    }

    #[test]
    fn a_human_decision_and_a_broken_client_are_told_apart() {
        let declined = classify(&ElicitationError::UserDeclined);
        let broken = classify(&parse_err());

        assert_eq!(declined.code, codes::NOT_CONFIRMED);
        assert_eq!(broken.code, codes::CONFIRMATION_FAILED);
        assert_ne!(declined.audit_detail(), broken.audit_detail());
    }

    #[test]
    fn declining_and_dismissing_share_a_code_but_not_an_audit_detail() {
        let declined = classify(&ElicitationError::UserDeclined);
        let cancelled = classify(&ElicitationError::UserCancelled);

        assert_eq!(declined.code, cancelled.code);
        assert_eq!(declined.audit_detail(), "not_confirmed:declined");
        assert_eq!(cancelled.audit_detail(), "not_confirmed:cancelled");
    }

    /// The pre-change vocabulary was the bare code, so both must still match.
    #[test]
    fn a_refusal_audits_under_its_code_whether_or_not_it_carries_a_reason() {
        for refusal in [
            classify(&ElicitationError::UserDeclined),
            declined("unchecked"),
        ] {
            assert!(
                refusal.audit_detail().starts_with(codes::NOT_CONFIRMED),
                "{} would not match a not_confirmed query",
                refusal.audit_detail()
            );
        }
    }

    /// An unanswered dialog is the client's silence, not a decision.
    #[test]
    fn a_timeout_is_not_recorded_as_a_human_saying_no() {
        let timed_out = classify(&ElicitationError::Service(ServiceError::Timeout {
            timeout: std::time::Duration::from_secs(30),
        }));

        assert_eq!(timed_out.code, codes::CONFIRMATION_FAILED);
        assert_eq!(timed_out.audit_detail(), "confirmation_failed:timed_out");
    }

    #[test]
    fn a_client_without_form_mode_is_reported_as_unsupported() {
        let mapped = classify(&ElicitationError::CapabilityNotSupported);

        assert_eq!(mapped.code, codes::ELICITATION_UNSUPPORTED);
    }

    #[test]
    fn every_refusal_is_final() {
        for err in [
            ElicitationError::UserDeclined,
            ElicitationError::UserCancelled,
            ElicitationError::NoContent,
            ElicitationError::CapabilityNotSupported,
            parse_err(),
        ] {
            assert!(!classify(&err).retryable, "{err:?} offered a retry");
        }
    }

    #[test]
    fn a_code_with_no_refinement_audits_as_itself() {
        assert_eq!(unsupported().audit_detail(), codes::ELICITATION_UNSUPPORTED);
    }

    #[test]
    fn the_clients_own_form_response_never_reaches_the_audit_trail() {
        let secret = "sk-live-must-not-be-logged";
        let mapped = classify(&ElicitationError::ParseError {
            error: serde_json::from_str::<u32>("\"x\"").unwrap_err(),
            data: serde_json::json!({ "confirm": secret }),
        });

        assert!(!mapped.audit_detail().contains(secret));
        assert!(!mapped.message.contains(secret));
    }
}
