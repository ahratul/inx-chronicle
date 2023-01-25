// Copyright 2023 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use time::{Duration, OffsetDateTime};

use super::{AddressCount, TransactionAnalytics};
use crate::types::{
    ledger::{LedgerOutput, LedgerSpent, MilestoneIndexTimestamp},
    stardust::block::Address,
};

/// Computes the number of addresses the currently hold a balance.
pub struct ActiveAddresses {
    start_time: OffsetDateTime,
    interval: Duration,
    addresses: HashSet<Address>,
    // Unfortunately, I don't see another way of implementing it using our current trait design
    flush: Option<usize>,
}

impl ActiveAddresses {
    /// Initialize the analytics be reading the current ledger state.
    pub fn init<'a>(
        start_time: OffsetDateTime,
        interval: Duration,
        unspent_outputs: impl Iterator<Item = &'a LedgerOutput>,
    ) -> Self {
        let addresses = unspent_outputs
            .filter_map(|output| {
                let booked = OffsetDateTime::try_from(output.booked.milestone_timestamp).unwrap();
                if (start_time <= booked) && (booked < start_time + interval) {
                    output.output.owning_address().cloned()
                } else {
                    None
                }
            })
            .collect();
        Self {
            start_time,
            interval,
            addresses,
            flush: None,
        }
    }
}

impl TransactionAnalytics for ActiveAddresses {
    type Measurement = AddressCount;

    fn begin_milestone(&mut self, at: MilestoneIndexTimestamp) {
        let end = self.start_time + self.interval;
        // Panic: The milestone timestamp is guaranteed to be valid.
        if OffsetDateTime::try_from(at.milestone_timestamp).unwrap() > end {
            self.flush = Some(self.addresses.len());
            self.addresses.clear();
            self.start_time = end;
        }
    }

    fn handle_transaction(&mut self, inputs: &[LedgerSpent], outputs: &[LedgerOutput]) {
        for input in inputs {
            if let Some(a) = input.output.output.owning_address() {
                self.addresses.insert(*a);
            }
        }

        for output in outputs {
            if let Some(a) = output.output.owning_address() {
                self.addresses.insert(*a);
            }
        }
    }

    fn end_milestone(&mut self, _: MilestoneIndexTimestamp) -> Option<Self::Measurement> {
        self.flush.take().map(AddressCount)
    }
}