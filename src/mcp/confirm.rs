//! Human-in-the-loop confirmation for write tools, via MCP elicitation.
//!
//! Before any mutation the tool calls [`require_confirmation`], which asks the
//! client (out of band of the tool arguments) to confirm. The write proceeds
//! only on an explicit `confirm = true`. Everything else — the user declines or
//! cancels, the client doesn't support elicitation, the response doesn't parse
//! — **fails closed**: no write fires. This is what stops a prompt-injected
//! "pause all my monitors" from executing without the human agreeing.

use rmcp::RoleServer;
use rmcp::service::RequestContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::{McpToolError, codes};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Confirmation {
    /// Set to true to approve this action. Anything else cancels it.
    confirm: bool,
}

rmcp::elicit_safe!(Confirmation);

/// Ask the user to approve `message`. `Ok(())` only on an explicit
/// `confirm = true`; every other outcome is a fail-closed `not_confirmed`.
pub async fn require_confirmation(
    ctx: &RequestContext<RoleServer>,
    message: impl Into<String>,
) -> Result<(), McpToolError> {
    match ctx.peer.elicit::<Confirmation>(message.into()).await {
        Ok(Some(c)) if c.confirm => Ok(()),
        _ => Err(McpToolError::new(
            codes::NOT_CONFIRMED,
            "the action was not confirmed (the user declined, or the client cannot \
             prompt for confirmation); no change was made",
            false,
        )),
    }
}
