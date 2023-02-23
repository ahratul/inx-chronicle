// Copyright 2023 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

//! Various analytics that give insight into the usage of the tangle.

use futures::TryStreamExt;
use thiserror::Error;

use self::{
    influx::PrepareQuery,
    ledger::{
        AddressActivityAnalytics, AddressActivityMeasurement, AddressBalancesAnalytics, BaseTokenActivityMeasurement,
        LedgerOutputMeasurement, LedgerSizeAnalytics, OutputActivityMeasurement, TransactionSizeMeasurement,
        UnclaimedTokenMeasurement, UnlockConditionMeasurement,
    },
    tangle::{BlockActivityMeasurement, MilestoneSizeMeasurement, ProtocolParamsMeasurement},
};
use crate::{
    db::{
        influxdb::{config::IntervalAnalyticsChoice, AnalyticsChoice, InfluxDb},
        MongoDb,
    },
    tangle::{BlockData, InputSource, Milestone},
    types::{
        ledger::{LedgerInclusionState, LedgerOutput, LedgerSpent, MilestoneIndexTimestamp},
        stardust::block::{payload::TransactionEssence, Input, Payload},
        tangle::{MilestoneIndex, ProtocolParameters},
    },
};

mod influx;
mod ledger;
mod tangle;

/// Provides an API to access basic information used for analytics
#[allow(missing_docs)]
pub trait AnalyticsContext: Send + Sync {
    fn protocol_params(&self) -> &ProtocolParameters;

    fn at(&self) -> &MilestoneIndexTimestamp;
}

impl<'a, I: InputSource> AnalyticsContext for Milestone<'a, I> {
    fn protocol_params(&self) -> &ProtocolParameters {
        &self.protocol_params
    }

    fn at(&self) -> &MilestoneIndexTimestamp {
        &self.at
    }
}

/// Defines how analytics are gathered.
pub trait Analytics {
    /// The resulting measurement.
    type Measurement;
    /// Handle a transaction consisting of inputs (consumed [`LedgerSpent`]) and outputs (created [`LedgerOutput`]).
    fn handle_transaction(
        &mut self,
        _consumed: &[LedgerSpent],
        _created: &[LedgerOutput],
        _ctx: &dyn AnalyticsContext,
    ) {
    }
    /// Handle a block.
    fn handle_block(&mut self, _block_data: &BlockData, _ctx: &dyn AnalyticsContext) {}
    /// Finish a milestone and return the measurement if one was created.
    fn end_milestone(&mut self, ctx: &dyn AnalyticsContext) -> Option<Self::Measurement>;
}

// This trait allows using the above implementation dynamically
trait DynAnalytics: Send {
    fn handle_transaction(&mut self, consumed: &[LedgerSpent], created: &[LedgerOutput], ctx: &dyn AnalyticsContext);
    fn handle_block(&mut self, block_data: &BlockData, ctx: &dyn AnalyticsContext);
    fn end_milestone(&mut self, ctx: &dyn AnalyticsContext) -> Option<Box<dyn PrepareQuery>>;
}

impl<T: Analytics + Send> DynAnalytics for T
where
    PerMilestone<T::Measurement>: 'static + PrepareQuery,
{
    fn handle_transaction(&mut self, consumed: &[LedgerSpent], created: &[LedgerOutput], ctx: &dyn AnalyticsContext) {
        Analytics::handle_transaction(self, consumed, created, ctx)
    }

    fn handle_block(&mut self, block_data: &BlockData, ctx: &dyn AnalyticsContext) {
        Analytics::handle_block(self, block_data, ctx)
    }

    fn end_milestone(&mut self, ctx: &dyn AnalyticsContext) -> Option<Box<dyn PrepareQuery>> {
        Analytics::end_milestone(self, ctx).map(|r| {
            Box::new(PerMilestone {
                at: *ctx.at(),
                inner: r,
            }) as _
        })
    }
}

#[async_trait::async_trait]
trait IntervalAnalytics {
    type Measurement;
    async fn handle_date_range(
        &mut self,
        start_date: time::Date,
        interval: AnalyticsInterval,
        db: &MongoDb,
    ) -> eyre::Result<Self::Measurement>;
}

// This trait allows using the above implementation dynamically
#[async_trait::async_trait]
trait DynIntervalAnalytics: Send {
    async fn handle_date_range(
        &mut self,
        start_date: time::Date,
        interval: AnalyticsInterval,
        db: &MongoDb,
    ) -> eyre::Result<Box<dyn PrepareQuery>>;
}

#[async_trait::async_trait]
impl<T: IntervalAnalytics + Send> DynIntervalAnalytics for T
where
    PerInterval<T::Measurement>: 'static + PrepareQuery,
{
    async fn handle_date_range(
        &mut self,
        start_date: time::Date,
        interval: AnalyticsInterval,
        db: &MongoDb,
    ) -> eyre::Result<Box<dyn PrepareQuery>> {
        IntervalAnalytics::handle_date_range(self, start_date, interval, db)
            .await
            .map(|r| {
                Box::new(PerInterval {
                    start_date,
                    interval,
                    inner: r,
                }) as _
            })
    }
}

#[allow(missing_docs)]
pub struct Analytic(Box<dyn DynAnalytics>);

impl Analytic {
    /// Init an analytic from a choice and ledger state.
    pub fn init<'a>(
        choice: &AnalyticsChoice,
        protocol_params: &ProtocolParameters,
        unspent_outputs: impl IntoIterator<Item = &'a LedgerOutput>,
    ) -> Self {
        Self(match choice {
            AnalyticsChoice::AddressBalance => Box::new(AddressBalancesAnalytics::init(unspent_outputs)) as _,
            AnalyticsChoice::BaseTokenActivity => Box::<BaseTokenActivityMeasurement>::default() as _,
            AnalyticsChoice::BlockActivity => Box::<BlockActivityMeasurement>::default() as _,
            AnalyticsChoice::ActiveAddresses => Box::<AddressActivityAnalytics>::default() as _,
            AnalyticsChoice::LedgerOutputs => Box::new(LedgerOutputMeasurement::init(unspent_outputs)) as _,
            AnalyticsChoice::LedgerSize => {
                Box::new(LedgerSizeAnalytics::init(protocol_params.clone(), unspent_outputs)) as _
            }
            AnalyticsChoice::MilestoneSize => Box::<MilestoneSizeMeasurement>::default() as _,
            AnalyticsChoice::OutputActivity => Box::<OutputActivityMeasurement>::default() as _,
            AnalyticsChoice::ProtocolParameters => Box::<ProtocolParamsMeasurement>::default() as _,
            AnalyticsChoice::TransactionSizeDistribution => Box::<TransactionSizeMeasurement>::default() as _,
            AnalyticsChoice::UnclaimedTokens => Box::new(UnclaimedTokenMeasurement::init(unspent_outputs)) as _,
            AnalyticsChoice::UnlockConditions => Box::new(UnlockConditionMeasurement::init(unspent_outputs)) as _,
        })
    }
}

impl<T: AsMut<[Analytic]>> Analytics for T {
    type Measurement = Vec<Box<dyn PrepareQuery>>;

    fn handle_block(&mut self, block_data: &BlockData, ctx: &dyn AnalyticsContext) {
        for analytic in self.as_mut().iter_mut() {
            analytic.0.handle_block(block_data, ctx);
        }
    }

    fn handle_transaction(&mut self, consumed: &[LedgerSpent], created: &[LedgerOutput], ctx: &dyn AnalyticsContext) {
        for analytic in self.as_mut().iter_mut() {
            analytic.0.handle_transaction(consumed, created, ctx);
        }
    }

    fn end_milestone(&mut self, ctx: &dyn AnalyticsContext) -> Option<Self::Measurement> {
        Some(
            self.as_mut()
                .iter_mut()
                .filter_map(|analytic| analytic.0.end_milestone(ctx))
                .collect(),
        )
    }
}

#[allow(missing_docs)]
pub struct IntervalAnalytic(Box<dyn DynIntervalAnalytics>);

impl IntervalAnalytic {
    /// Init an analytic from a choice and ledger state.
    pub fn init(choice: &IntervalAnalyticsChoice) -> Self {
        Self(match choice {
            IntervalAnalyticsChoice::ActiveAddresses => Box::<AddressActivityMeasurement>::default() as _,
        })
    }
}

#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum AnalyticsError {
    #[error("missing created output ({output_id}) in milestone {milestone_index}")]
    MissingLedgerOutput {
        output_id: String,
        milestone_index: MilestoneIndex,
    },
    #[error("missing consumed output ({output_id}) in milestone {milestone_index}")]
    MissingLedgerSpent {
        output_id: String,
        milestone_index: MilestoneIndex,
    },
}

impl<'a, I: InputSource> Milestone<'a, I> {
    /// Update a list of analytics with this milestone
    pub async fn update_analytics<A: Analytics + Send>(
        &self,
        analytics: &mut A,
        influxdb: &InfluxDb,
    ) -> eyre::Result<()>
    where
        PerMilestone<A::Measurement>: 'static + PrepareQuery,
    {
        let mut cone_stream = self.cone_stream().await?;

        while let Some(block_data) = cone_stream.try_next().await? {
            self.handle_block(analytics, &block_data)?;
        }

        self.end_milestone(analytics, influxdb).await?;

        Ok(())
    }

    fn handle_block<A: Analytics + Send>(&self, analytics: &mut A, block_data: &BlockData) -> eyre::Result<()> {
        if block_data.metadata.inclusion_state == LedgerInclusionState::Included {
            if let Some(Payload::Transaction(payload)) = &block_data.block.payload {
                let TransactionEssence::Regular { inputs, outputs, .. } = &payload.essence;
                let consumed = inputs
                    .iter()
                    .filter_map(|input| match input {
                        Input::Utxo(output_id) => Some(output_id),
                        _ => None,
                    })
                    .map(|output_id| {
                        Ok(self
                            .ledger_updates()
                            .get_consumed(output_id)
                            .ok_or(AnalyticsError::MissingLedgerSpent {
                                output_id: output_id.to_hex(),
                                milestone_index: block_data.metadata.referenced_by_milestone_index,
                            })?
                            .clone())
                    })
                    .collect::<eyre::Result<Vec<_>>>()?;
                let created = outputs
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let output_id = (payload.transaction_id, index as _).into();
                        Ok(self
                            .ledger_updates()
                            .get_created(&output_id)
                            .ok_or(AnalyticsError::MissingLedgerOutput {
                                output_id: output_id.to_hex(),
                                milestone_index: block_data.metadata.referenced_by_milestone_index,
                            })?
                            .clone())
                    })
                    .collect::<eyre::Result<Vec<_>>>()?;
                analytics.handle_transaction(&consumed, &created, self)
            }
        }
        analytics.handle_block(block_data, self);
        Ok(())
    }

    async fn end_milestone(&self, analytics: &mut impl DynAnalytics, influxdb: &InfluxDb) -> eyre::Result<()> {
        if let Some(measurement) = analytics.end_milestone(self) {
            influxdb.insert_measurement(measurement).await?;
        }
        Ok(())
    }
}

impl MongoDb {
    /// Update a list of interval analytics with this date.
    pub async fn update_interval_analytics(
        &self,
        analytics: &mut [IntervalAnalytic],
        influxdb: &InfluxDb,
        start: time::Date,
        interval: AnalyticsInterval,
    ) -> eyre::Result<()> {
        for analytic in analytics {
            influxdb
                .insert_measurement(analytic.0.handle_date_range(start, interval, self).await?)
                .await?;
        }
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
#[allow(missing_docs)]
pub enum AnalyticsInterval {
    Day,
    Week,
    Month,
    Year,
}

impl AnalyticsInterval {
    /// Get the duration based on the start date and interval.
    pub fn to_duration(&self, start_date: &time::Date) -> time::Duration {
        match self {
            AnalyticsInterval::Day => time::Duration::days(1),
            AnalyticsInterval::Week => time::Duration::days(7),
            AnalyticsInterval::Month => {
                time::Duration::days(time::util::days_in_year_month(start_date.year(), start_date.month()) as _)
            }
            AnalyticsInterval::Year => time::Duration::days(time::util::days_in_year(start_date.year()) as _),
        }
    }

    /// Get the exclusive end date based on the start date and interval.
    pub fn end_date(&self, start_date: &time::Date) -> time::Date {
        *start_date + self.to_duration(start_date)
    }
}

impl std::fmt::Display for AnalyticsInterval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                AnalyticsInterval::Day => "daily",
                AnalyticsInterval::Week => "weekly",
                AnalyticsInterval::Month => "monthly",
                AnalyticsInterval::Year => "yearly",
            }
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct SyncAnalytics {
    pub sync_time: u64,
}

#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PerMilestone<M> {
    at: MilestoneIndexTimestamp,
    inner: M,
}

#[derive(Clone, Debug)]
#[allow(missing_docs)]
struct PerInterval<M> {
    start_date: time::Date,
    interval: AnalyticsInterval,
    inner: M,
}

#[cfg(test)]
mod test {
    use std::{fs::File, io::BufReader};

    use futures::TryStreamExt;
    use serde::{Deserialize, Serialize};

    use super::{
        ledger::{
            AddressActivityAnalytics, AddressActivityMeasurement, AddressBalanceMeasurement,
            BaseTokenActivityMeasurement, LedgerSizeMeasurement, OutputActivityMeasurement, TransactionSizeMeasurement,
        },
        tangle::{BlockActivityMeasurement, MilestoneSizeMeasurement},
        Analytics, AnalyticsContext,
    };
    use crate::{
        analytics::ledger::{
            AddressBalancesAnalytics, LedgerOutputMeasurement, LedgerSizeAnalytics, UnclaimedTokenMeasurement,
            UnlockConditionMeasurement,
        },
        tangle::{get_in_memory_data, IN_MEM_MILESTONE},
        types::{ledger::MilestoneIndexTimestamp, tangle::ProtocolParameters},
    };

    pub(crate) struct TestContext {
        pub(crate) at: MilestoneIndexTimestamp,
        pub(crate) params: ProtocolParameters,
    }

    impl AnalyticsContext for TestContext {
        fn protocol_params(&self) -> &ProtocolParameters {
            &self.params
        }

        fn at(&self) -> &MilestoneIndexTimestamp {
            &self.at
        }
    }

    #[derive(Serialize, Deserialize)]
    struct TestAnalytics {
        active_addresses: AddressActivityAnalytics,
        address_balance: AddressBalancesAnalytics,
        base_tokens: BaseTokenActivityMeasurement,
        ledger_outputs: LedgerOutputMeasurement,
        ledger_size: LedgerSizeAnalytics,
        output_activity: OutputActivityMeasurement,
        transaction_size: TransactionSizeMeasurement,
        unclaimed_tokens: UnclaimedTokenMeasurement,
        unlock_conditions: UnlockConditionMeasurement,
        block_activity: BlockActivityMeasurement,
        milestone_size: MilestoneSizeMeasurement,
    }

    impl TestAnalytics {
        #[allow(dead_code)]
        fn init<'a>(
            protocol_params: ProtocolParameters,
            unspent_outputs: impl IntoIterator<Item = &'a crate::types::ledger::LedgerOutput> + Copy,
        ) -> Self {
            Self {
                active_addresses: Default::default(),
                address_balance: AddressBalancesAnalytics::init(unspent_outputs),
                base_tokens: Default::default(),
                ledger_outputs: LedgerOutputMeasurement::init(unspent_outputs),
                ledger_size: LedgerSizeAnalytics::init(protocol_params, unspent_outputs),
                output_activity: Default::default(),
                transaction_size: Default::default(),
                unclaimed_tokens: UnclaimedTokenMeasurement::init(unspent_outputs),
                unlock_conditions: UnlockConditionMeasurement::init(unspent_outputs),
                block_activity: Default::default(),
                milestone_size: Default::default(),
            }
        }
    }

    #[derive(Debug)]
    struct TestMeasurements {
        active_addresses: AddressActivityMeasurement,
        address_balance: AddressBalanceMeasurement,
        base_tokens: BaseTokenActivityMeasurement,
        ledger_outputs: LedgerOutputMeasurement,
        ledger_size: LedgerSizeMeasurement,
        output_activity: OutputActivityMeasurement,
        transaction_size: TransactionSizeMeasurement,
        unclaimed_tokens: UnclaimedTokenMeasurement,
        unlock_conditions: UnlockConditionMeasurement,
        block_activity: BlockActivityMeasurement,
        milestone_size: MilestoneSizeMeasurement,
    }

    impl Analytics for TestAnalytics {
        type Measurement = TestMeasurements;

        fn handle_block(&mut self, block_data: &crate::tangle::BlockData, ctx: &dyn AnalyticsContext) {
            self.active_addresses.handle_block(block_data, ctx);
            self.address_balance.handle_block(block_data, ctx);
            self.base_tokens.handle_block(block_data, ctx);
            self.ledger_outputs.handle_block(block_data, ctx);
            self.ledger_size.handle_block(block_data, ctx);
            self.output_activity.handle_block(block_data, ctx);
            self.transaction_size.handle_block(block_data, ctx);
            self.unclaimed_tokens.handle_block(block_data, ctx);
            self.unlock_conditions.handle_block(block_data, ctx);
            self.block_activity.handle_block(block_data, ctx);
            self.milestone_size.handle_block(block_data, ctx);
        }

        fn handle_transaction(
            &mut self,
            consumed: &[crate::types::ledger::LedgerSpent],
            created: &[crate::types::ledger::LedgerOutput],
            ctx: &dyn AnalyticsContext,
        ) {
            self.active_addresses.handle_transaction(consumed, created, ctx);
            self.address_balance.handle_transaction(consumed, created, ctx);
            self.base_tokens.handle_transaction(consumed, created, ctx);
            self.ledger_outputs.handle_transaction(consumed, created, ctx);
            self.ledger_size.handle_transaction(consumed, created, ctx);
            self.output_activity.handle_transaction(consumed, created, ctx);
            self.transaction_size.handle_transaction(consumed, created, ctx);
            self.unclaimed_tokens.handle_transaction(consumed, created, ctx);
            self.unlock_conditions.handle_transaction(consumed, created, ctx);
            self.block_activity.handle_transaction(consumed, created, ctx);
            self.milestone_size.handle_transaction(consumed, created, ctx);
        }

        fn end_milestone(&mut self, ctx: &dyn AnalyticsContext) -> Option<Self::Measurement> {
            Some(TestMeasurements {
                active_addresses: self.active_addresses.end_milestone(ctx).unwrap(),
                address_balance: self.address_balance.end_milestone(ctx).unwrap(),
                base_tokens: self.base_tokens.end_milestone(ctx).unwrap(),
                ledger_outputs: self.ledger_outputs.end_milestone(ctx).unwrap(),
                ledger_size: self.ledger_size.end_milestone(ctx).unwrap(),
                output_activity: self.output_activity.end_milestone(ctx).unwrap(),
                transaction_size: self.transaction_size.end_milestone(ctx).unwrap(),
                unclaimed_tokens: self.unclaimed_tokens.end_milestone(ctx).unwrap(),
                unlock_conditions: self.unlock_conditions.end_milestone(ctx).unwrap(),
                block_activity: self.block_activity.end_milestone(ctx).unwrap(),
                milestone_size: self.milestone_size.end_milestone(ctx).unwrap(),
            })
        }
    }

    #[tokio::test]
    async fn test_in_memory_analytics() {
        let analytics = gather_in_memory_analytics().await.unwrap();
        assert_eq!(analytics.active_addresses.count, 32);

        assert_eq!(analytics.address_balance.address_with_balance_count, 111983);

        assert_eq!(analytics.base_tokens.booked_amount.0, 96847628508);
        assert_eq!(analytics.base_tokens.transferred_amount.0, 95428996456);

        assert_eq!(analytics.ledger_outputs.basic.count, 99398);
        assert_eq!(analytics.ledger_outputs.basic.amount.0, 1813618032119665);
        assert_eq!(analytics.ledger_outputs.alias.count, 40);
        assert_eq!(analytics.ledger_outputs.alias.amount.0, 2083400);
        assert_eq!(analytics.ledger_outputs.nft.count, 14948);
        assert_eq!(analytics.ledger_outputs.nft.amount.0, 2473025700);
        assert_eq!(analytics.ledger_outputs.foundry.count, 28);
        assert_eq!(analytics.ledger_outputs.foundry.amount.0, 1832600);

        assert_eq!(analytics.ledger_size.total_key_bytes, 3890076);
        assert_eq!(analytics.ledger_size.total_data_bytes, 28233855);
        assert_eq!(analytics.ledger_size.total_storage_deposit_amount.0, 6713461500);

        assert_eq!(analytics.output_activity.nft.created_count, 22);
        assert_eq!(analytics.output_activity.nft.transferred_count, 1);
        assert_eq!(analytics.output_activity.nft.destroyed_count, 0);
        assert_eq!(analytics.output_activity.alias.created_count, 0);
        assert_eq!(analytics.output_activity.alias.governor_changed_count, 0);
        assert_eq!(analytics.output_activity.alias.state_changed_count, 1);
        assert_eq!(analytics.output_activity.alias.destroyed_count, 0);
        assert_eq!(analytics.output_activity.foundry.created_count, 0);
        assert_eq!(analytics.output_activity.foundry.transferred_count, 0);
        assert_eq!(analytics.output_activity.foundry.destroyed_count, 0);

        assert_eq!(analytics.transaction_size.input_buckets.single(1), 2);
        assert_eq!(analytics.transaction_size.input_buckets.single(2), 0);
        assert_eq!(analytics.transaction_size.input_buckets.single(3), 2);
        assert_eq!(analytics.transaction_size.input_buckets.single(4), 1);
        assert_eq!(analytics.transaction_size.input_buckets.single(5), 0);
        assert_eq!(analytics.transaction_size.input_buckets.single(6), 0);
        assert_eq!(analytics.transaction_size.input_buckets.single(7), 0);
        assert_eq!(analytics.transaction_size.input_buckets.small, 0);
        assert_eq!(analytics.transaction_size.input_buckets.medium, 0);
        assert_eq!(analytics.transaction_size.input_buckets.large, 0);
        assert_eq!(analytics.transaction_size.input_buckets.huge, 0);
        assert_eq!(analytics.transaction_size.output_buckets.single(1), 1);
        assert_eq!(analytics.transaction_size.output_buckets.single(2), 3);
        assert_eq!(analytics.transaction_size.output_buckets.single(3), 0);
        assert_eq!(analytics.transaction_size.output_buckets.single(4), 0);
        assert_eq!(analytics.transaction_size.output_buckets.single(5), 0);
        assert_eq!(analytics.transaction_size.output_buckets.single(6), 0);
        assert_eq!(analytics.transaction_size.output_buckets.single(7), 0);
        assert_eq!(analytics.transaction_size.output_buckets.small, 0);
        assert_eq!(analytics.transaction_size.output_buckets.medium, 1);
        assert_eq!(analytics.transaction_size.output_buckets.large, 0);
        assert_eq!(analytics.transaction_size.output_buckets.huge, 0);

        assert_eq!(analytics.unclaimed_tokens.unclaimed_count, 90018);
        assert_eq!(analytics.unclaimed_tokens.unclaimed_amount.0, 1672822033755291);

        assert_eq!(analytics.unlock_conditions.expiration.count, 189);
        assert_eq!(analytics.unlock_conditions.expiration.amount.0, 256841968260);
        assert_eq!(analytics.unlock_conditions.timelock.count, 0);
        assert_eq!(analytics.unlock_conditions.timelock.amount.0, 0);
        assert_eq!(analytics.unlock_conditions.storage_deposit_return.count, 449);
        assert_eq!(analytics.unlock_conditions.storage_deposit_return.amount.0, 3987025775);
        assert_eq!(
            analytics.unlock_conditions.storage_deposit_return_inner_amount,
            22432909
        );

        assert_eq!(analytics.block_activity.milestone_count, 1);
        assert_eq!(analytics.block_activity.no_payload_count, 0);
        assert_eq!(analytics.block_activity.tagged_data_count, 32);
        assert_eq!(analytics.block_activity.transaction_count, 5);
        assert_eq!(analytics.block_activity.treasury_transaction_count, 0);
        assert_eq!(analytics.block_activity.confirmed_count, 5);
        assert_eq!(analytics.block_activity.conflicting_count, 0);
        assert_eq!(analytics.block_activity.no_transaction_count, 33);

        assert_eq!(analytics.milestone_size.total_milestone_payload_bytes, 1482);
        assert_eq!(analytics.milestone_size.total_tagged_data_payload_bytes, 8352);
        assert_eq!(analytics.milestone_size.total_transaction_payload_bytes, 34063);
        assert_eq!(analytics.milestone_size.total_treasury_transaction_payload_bytes, 0);
        assert_eq!(analytics.milestone_size.total_milestone_bytes, 43897);
    }

    async fn gather_in_memory_analytics() -> eyre::Result<TestMeasurements> {
        let data = get_in_memory_data();
        let mut stream = data.milestone_stream(IN_MEM_MILESTONE..=IN_MEM_MILESTONE).await?;
        let mut res = None;
        while let Some(milestone) = stream.try_next().await? {
            let file = File::open(format!("tests/data/ledger_state_ms_{IN_MEM_MILESTONE}_analytics.ron"))?;
            let mut analytics: TestAnalytics = ron::de::from_reader(BufReader::new(file))?;
            let mut cone_stream = milestone.cone_stream().await?;

            while let Some(block_data) = cone_stream.try_next().await? {
                milestone.handle_block(&mut analytics, &block_data)?;
            }

            res = analytics.end_milestone(&milestone);
        }

        Ok(res.unwrap())
    }
}
