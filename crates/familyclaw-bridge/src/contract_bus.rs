//! Contract messages over the bus: a transport-independent serde protocol.
//!
//! This module defines the [`ContractMessage`] enum, which lets contract
//! lifecycle events (propose/accept/reject/fulfill/fail/breach) be serialized
//! and carried over any bus, and published as
//! [`crate::event::EventKind::Custom`] events under the name
//! [`CONTRACT_CUSTOM_NAME`].
//!
//! **Important boundary:** this is *pure serde* only — there is **NO**
//! dependency on the `familyclaw-bus` crate, and no binding to the Resonance
//! Bus / Ractor layer. An adapter can bridge these messages onto the actual
//! bus later.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::ids::{AgentId, MessageId};

use crate::contract::{Contract, ContractStatus, Deliverable};

/// The stable custom-event name under which contract messages are published
/// to the bus.
///
/// Versioned (`.v1`) so a later protocol change can coexist under its own
/// name without breaking existing consumers.
pub const CONTRACT_CUSTOM_NAME: &str = "familyclaw.contract.v1";

/// A contract lifecycle message carried over the bus.
///
/// The serde representation uses an internal tag with the key `op`, making
/// the message readable and forward-compatible (e.g. `{"op":"propose", ...}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ContractMessage {
    /// The requester proposes a contract to the provider.
    Propose {
        /// The contract's identifier.
        contract_id: MessageId,
        /// The requester.
        requester: AgentId,
        /// The provider.
        provider: AgentId,
        /// The capability's name.
        capability: String,
        /// The contract's input.
        input: Value,
    },

    /// The provider accepts the proposal.
    Accept {
        /// The contract's identifier.
        contract_id: MessageId,
        /// The accepting provider.
        provider: AgentId,
    },

    /// The provider rejects the proposal with the given reason.
    Reject {
        /// The contract's identifier.
        contract_id: MessageId,
        /// The rejecting provider.
        provider: AgentId,
        /// The reason for rejection.
        reason: String,
    },

    /// The provider fulfills the contract with a deliverable.
    Fulfill {
        /// The contract's identifier.
        contract_id: MessageId,
        /// The deliverable.
        deliverable: Deliverable,
    },

    /// The provider reports that it cannot deliver.
    Fail {
        /// The contract's identifier.
        contract_id: MessageId,
        /// The reason for failure.
        reason: String,
    },

    /// The verifier reports that the deliverable breached the output
    /// schema/postcondition.
    Breach {
        /// The contract's identifier.
        contract_id: MessageId,
        /// A description of the breached condition/schema.
        detail: String,
    },
}

impl ContractMessage {
    /// Returns the identifier of the contract this message concerns.
    #[must_use]
    pub fn contract_id(&self) -> MessageId {
        match self {
            ContractMessage::Propose { contract_id, .. }
            | ContractMessage::Accept { contract_id, .. }
            | ContractMessage::Reject { contract_id, .. }
            | ContractMessage::Fulfill { contract_id, .. }
            | ContractMessage::Fail { contract_id, .. }
            | ContractMessage::Breach { contract_id, .. } => *contract_id,
        }
    }

    /// Returns the operation's stable name (same as the serde tag).
    #[must_use]
    pub fn op(&self) -> &'static str {
        match self {
            ContractMessage::Propose { .. } => "propose",
            ContractMessage::Accept { .. } => "accept",
            ContractMessage::Reject { .. } => "reject",
            ContractMessage::Fulfill { .. } => "fulfill",
            ContractMessage::Fail { .. } => "fail",
            ContractMessage::Breach { .. } => "breach",
        }
    }

    /// Builds a `Propose` message from a contract.
    #[must_use]
    pub fn propose_from(contract: &Contract) -> Self {
        ContractMessage::Propose {
            contract_id: contract.id,
            requester: contract.requester,
            provider: contract.provider,
            capability: contract.capability.name.clone(),
            input: contract.input.clone(),
        }
    }

    /// Builds a status-appropriate notification message from a contract, if
    /// one is natural (e.g. `Fulfilled` → a `Fulfill` message with the
    /// deliverable).
    ///
    /// Returns `None` for states that are not notified this way (`Proposed`,
    /// `Accepted`), which travel via their own messages.
    #[must_use]
    pub fn from_contract_status(contract: &Contract) -> Option<Self> {
        match contract.status {
            ContractStatus::Fulfilled => {
                contract
                    .deliverable
                    .clone()
                    .map(|deliverable| ContractMessage::Fulfill {
                        contract_id: contract.id,
                        deliverable,
                    })
            }
            ContractStatus::Rejected => Some(ContractMessage::Reject {
                contract_id: contract.id,
                provider: contract.provider,
                reason: "rejected".to_string(),
            }),
            ContractStatus::Failed => Some(ContractMessage::Fail {
                contract_id: contract.id,
                reason: "failed".to_string(),
            }),
            ContractStatus::Proposed | ContractStatus::Accepted => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn custom_name_is_versioned() {
        assert_eq!(CONTRACT_CUSTOM_NAME, "familyclaw.contract.v1");
    }

    #[test]
    fn propose_serde_roundtrip_with_op_tag() {
        let msg = ContractMessage::Propose {
            contract_id: MessageId::new(),
            requester: AgentId::new(),
            provider: AgentId::new(),
            capability: "render_video".into(),
            input: json!({ "script": "s", "duration": 5 }),
        };
        let text = serde_json::to_string(&msg).expect("serialize");
        // Internal tag `op`.
        let v: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(v["op"], json!("propose"));

        let back: ContractMessage = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn all_variants_roundtrip() {
        let id = MessageId::new();
        let provider = AgentId::new();
        let deliverable = Deliverable::new(
            provider,
            json!({ "url": "u", "frames": 1 }),
            familyclaw_core::time::from_unix_secs(10).expect("ts"),
        );
        let messages = vec![
            ContractMessage::Accept {
                contract_id: id,
                provider,
            },
            ContractMessage::Reject {
                contract_id: id,
                provider,
                reason: "busy".into(),
            },
            ContractMessage::Fulfill {
                contract_id: id,
                deliverable,
            },
            ContractMessage::Fail {
                contract_id: id,
                reason: "oom".into(),
            },
            ContractMessage::Breach {
                contract_id: id,
                detail: "len(url) >= 1".into(),
            },
        ];
        for msg in messages {
            let text = serde_json::to_string(&msg).expect("serialize");
            let back: ContractMessage = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(msg, back);
            assert_eq!(back.contract_id(), id);
            assert!(!back.op().is_empty());
        }
    }

    #[test]
    fn op_matches_serde_tag() {
        let msg = ContractMessage::Breach {
            contract_id: MessageId::new(),
            detail: "x".into(),
        };
        let v: Value = serde_json::to_value(&msg).expect("to_value");
        assert_eq!(v["op"], json!(msg.op()));
    }
}
