// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use bee_block_stardust::payload as bee;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod milestone;
pub mod tagged_data;
pub mod transaction;
pub mod treasury_transaction;

pub use self::{
    milestone::{MilestoneId, MilestonePayload},
    tagged_data::TaggedDataPayload,
    transaction::{TransactionEssence, TransactionId, TransactionPayload},
    treasury_transaction::TreasuryTransactionPayload,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Payload {
    Transaction(Box<TransactionPayload>),
    Milestone(Box<MilestonePayload>),
    TreasuryTransaction(Box<TreasuryTransactionPayload>),
    TaggedData(Box<TaggedDataPayload>),
}

impl Payload {
    pub fn kind(&self) -> &'static str {
        match self {
            Payload::Transaction(_) => "transaction",
            Payload::Milestone(_) => "milestone",
            Payload::TreasuryTransaction(_) => "treasury_transaction",
            Payload::TaggedData(_) => "tagged_data",
        }
    }
}

impl From<&bee::Payload> for Payload {
    fn from(value: &bee::Payload) -> Self {
        match value {
            bee::Payload::Transaction(p) => Self::Transaction(Box::new(p.as_ref().into())),
            bee::Payload::Milestone(p) => Self::Milestone(Box::new(p.as_ref().into())),
            bee::Payload::TreasuryTransaction(p) => Self::TreasuryTransaction(Box::new(p.as_ref().into())),
            bee::Payload::TaggedData(p) => Self::TaggedData(Box::new(p.as_ref().into())),
        }
    }
}

impl TryFrom<Payload> for bee::Payload {
    type Error = bee_block_stardust::Error;

    fn try_from(value: Payload) -> Result<Self, Self::Error> {
        Ok(match value {
            Payload::Transaction(p) => bee::Payload::Transaction(Box::new((*p).try_into()?)),
            Payload::Milestone(p) => bee::Payload::Milestone(Box::new((*p).try_into()?)),
            Payload::TreasuryTransaction(p) => bee::Payload::TreasuryTransaction(Box::new((*p).try_into()?)),
            Payload::TaggedData(p) => bee::Payload::TaggedData(Box::new((*p).try_into()?)),
        })
    }
}

#[derive(Debug, Error)]
#[error("wrong payload requested. expected {expected}, found: {found}")]
pub struct WrongPayloadError {
    expected: &'static str,
    found: &'static str,
}

macro_rules! impl_coerce_payload {
    ($kind:literal, $t:ty, $var:ident) => {
        impl TryFrom<Payload> for $t {
            type Error = WrongPayloadError;

            fn try_from(value: Payload) -> Result<Self, Self::Error> {
                if let Payload::$var(payload) = value {
                    Ok(*payload)
                } else {
                    Err(WrongPayloadError {
                        expected: $kind,
                        found: value.kind(),
                    })
                }
            }
        }
    };
}
impl_coerce_payload!("transaction", TransactionPayload, Transaction);
impl_coerce_payload!("milestone", MilestonePayload, Milestone);
impl_coerce_payload!("treasury_transaction", TreasuryTransactionPayload, TreasuryTransaction);
impl_coerce_payload!("tagged_data", TaggedDataPayload, TaggedData);

#[cfg(test)]
mod test {
    use mongodb::bson::{doc, from_bson, to_bson, to_document};

    use crate::types::stardust::{
        block::{payload::TransactionEssence, Payload},
        util::payload::*,
    };

    #[test]
    fn test_payload_bson() {
        let payload = get_test_transaction_payload();
        let mut bson = to_bson(&payload).unwrap();
        // Need to re-add outputs as they are not serialized
        let outputs_doc = if let Payload::Transaction(payload) = &payload {
            let TransactionEssence::Regular { outputs, .. } = &payload.essence;
            doc! { "outputs": outputs.iter().map(to_document).collect::<Result<Vec<_>, _>>().unwrap() }
        } else {
            unreachable!();
        };
        let doc = bson.as_document_mut().unwrap().get_document_mut("essence").unwrap();
        doc.extend(outputs_doc);
        assert_eq!(payload, from_bson::<Payload>(bson).unwrap());

        let payload = get_test_milestone_payload();
        let bson = to_bson(&payload).unwrap();
        assert_eq!(payload, from_bson::<Payload>(bson).unwrap());

        let payload = get_test_treasury_transaction_payload();
        let bson = to_bson(&payload).unwrap();
        assert_eq!(payload, from_bson::<Payload>(bson).unwrap());

        let payload = get_test_tagged_data_payload();
        let bson = to_bson(&payload).unwrap();
        assert_eq!(payload, from_bson::<Payload>(bson).unwrap());
    }
}
