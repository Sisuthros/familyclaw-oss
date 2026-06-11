//! Sopimusviestit väylän yli: kuljetuksesta riippumaton serde-protokolla.
//!
//! Tämä moduuli määrittelee [`ContractMessage`]-enumin, jolla sopimuksen
//! elinkaaren tapahtumat (ehdota/hyväksy/hylkää/täytä/epäonnistu/rikkomus)
//! voidaan sarjallistaa ja kuljettaa minkä tahansa väylän yli ja julkaista
//! [`crate::event::EventKind::Custom`]-tapahtumina nimellä
//! [`CONTRACT_CUSTOM_NAME`].
//!
//! **Tärkeä rajaus:** tässä on vain *puhdas serde* — **EI** riippuvuutta
//! `familyclaw-bus`-cratesta eikä mitään Resonance Bus / Ractor -sidontaa.
//! Adapteri voi sillata nämä viestit varsinaiseen väylään myöhemmin.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::ids::{AgentId, MessageId};

use crate::contract::{Contract, ContractStatus, Deliverable};

/// Custom-tapahtuman vakaa nimi jolla sopimusviestit julkaistaan väylälle.
///
/// Versioitu (`.v1`) jotta protokollan myöhempi muutos voi rinnastua omaan
/// nimeensä rikkomatta vanhoja kuluttajia.
pub const CONTRACT_CUSTOM_NAME: &str = "familyclaw.contract.v1";

/// Sopimuksen elinkaaren viesti väylän yli.
///
/// Serde-esitys käyttää sisäistä tagia avaimella `op`, jolloin viesti on
/// luettava ja eteenpäin-yhteensopiva (esim. `{"op":"propose", ...}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ContractMessage {
    /// Pyytäjä ehdottaa sopimusta tarjoajalle.
    Propose {
        /// Sopimuksen tunniste.
        contract_id: MessageId,
        /// Pyytäjä.
        requester: AgentId,
        /// Tarjoaja.
        provider: AgentId,
        /// Kyvyn nimi.
        capability: String,
        /// Sopimuksen syöte.
        input: Value,
    },

    /// Tarjoaja hyväksyy ehdotuksen.
    Accept {
        /// Sopimuksen tunniste.
        contract_id: MessageId,
        /// Hyväksyvä tarjoaja.
        provider: AgentId,
    },

    /// Tarjoaja hylkää ehdotuksen annetulla syyllä.
    Reject {
        /// Sopimuksen tunniste.
        contract_id: MessageId,
        /// Hylkäävä tarjoaja.
        provider: AgentId,
        /// Hylkäyksen syy.
        reason: String,
    },

    /// Tarjoaja täyttää sopimuksen toimitteella.
    Fulfill {
        /// Sopimuksen tunniste.
        contract_id: MessageId,
        /// Toimite.
        deliverable: Deliverable,
    },

    /// Tarjoaja ilmoittaa ettei pysty toimittamaan.
    Fail {
        /// Sopimuksen tunniste.
        contract_id: MessageId,
        /// Epäonnistumisen syy.
        reason: String,
    },

    /// Todentaja ilmoittaa että toimite rikkoi tulosskeeman/jälkiehdon.
    Breach {
        /// Sopimuksen tunniste.
        contract_id: MessageId,
        /// Rikotun ehdon/skeeman kuvaus.
        detail: String,
    },
}

impl ContractMessage {
    /// Palauttaa viestin koskeman sopimuksen tunnisteen.
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

    /// Palauttaa operaation vakaan nimen (sama kuin serde-tagi).
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

    /// Rakentaa `Propose`-viestin sopimuksesta.
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

    /// Rakentaa tilan mukaisen ilmoitusviestin sopimuksesta, jos sellainen on
    /// luonteva (esim. `Fulfilled` → `Fulfill`-viesti toimitteen kanssa).
    ///
    /// Palauttaa `None` ei-ilmoitettaville tiloille (`Proposed`, `Accepted`),
    /// jotka kulkevat omilla viesteillään.
    #[must_use]
    pub fn from_contract_status(contract: &Contract) -> Option<Self> {
        match contract.status {
            ContractStatus::Fulfilled => contract.deliverable.clone().map(|deliverable| {
                ContractMessage::Fulfill {
                    contract_id: contract.id,
                    deliverable,
                }
            }),
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
        // Sisäinen tagi `op`.
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
