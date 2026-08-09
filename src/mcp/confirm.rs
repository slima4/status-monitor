//! Human-in-the-loop confirmation for write tools, via MCP elicitation.
//!
//! Before any mutation the tool calls [`require_confirmation`], which asks the
//! client (out of band of the tool arguments) to confirm. The write proceeds
//! only on an explicit `confirm = true`. Everything else — the user declines or
//! cancels, the client doesn't support elicitation, the response doesn't parse
//! — **fails closed**: no write fires. This is what stops a prompt-injected
//! "pause all my monitors" from executing without the human agreeing.
//!
//! A client that never negotiated elicitation is told so by its own code
//! ([`codes::ELICITATION_UNSUPPORTED`]) instead of the code for "the user said
//! no": the two need different fixes, and only one of them is the caller's.

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

/// True when the connected client negotiated elicitation at `initialize`, so a
/// write tool can ask for confirmation. A peer that never initialized (a bare
/// `tools/list` probe) reports `None`, which is unknown, not unsupported — the
/// elicit call itself is the fail-closed backstop there.
pub fn client_can_confirm(ctx: &RequestContext<RoleServer>) -> bool {
    ctx.peer
        .peer_info()
        .is_none_or(|info| info.capabilities.elicitation.is_some())
}

/// Ask the user to approve `message`. `Ok(())` only on an explicit
/// `confirm = true`; every other outcome fails closed.
pub async fn require_confirmation(
    ctx: &RequestContext<RoleServer>,
    message: impl Into<String>,
) -> Result<(), McpToolError> {
    if !client_can_confirm(ctx) {
        return Err(McpToolError::new(
            codes::ELICITATION_UNSUPPORTED,
            "this MCP client cannot prompt for confirmation, and no change is made \
             without one; run the connector from a client that supports elicitation, \
             or make this change in the Uptimepage app",
            false,
        ));
    }
    match ctx.peer.elicit::<Confirmation>(message.into()).await {
        Ok(Some(c)) if c.confirm => Ok(()),
        _ => Err(McpToolError::new(
            codes::NOT_CONFIRMED,
            "the action was not confirmed; no change was made",
            false,
        )),
    }
}
