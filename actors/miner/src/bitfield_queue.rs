// Copyright 2019-2022 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use std::collections::HashMap;
use std::convert::TryInto;

use actors_runtime::{ActorDowncast, Array};
use bitfield::BitField;
use cid::Cid;
use fvm_shared::blockstore::Blockstore;
use fvm_shared::clock::{ChainEpoch, QuantSpec};
use ipld_amt::Error as AmtError;

/// Wrapper for working with an AMT[ChainEpoch]*Bitfield functioning as a queue, bucketed by epoch.
/// Keys in the queue are quantized (upwards), modulo some offset, to reduce the cardinality of keys.
pub struct BitFieldQueue<'db, BS> {
    pub amt: Array<'db, BitField, BS>,
    quant: QuantSpec,
}

impl<'db, BS: Blockstore> BitFieldQueue<'db, BS> {
    pub fn new(store: &'db BS, root: &Cid, quant: QuantSpec) -> Result<Self, AmtError> {
        Ok(Self {
            amt: Array::load(root, store)?,
            quant,
        })
    }

    /// Adds values to the queue entry for an epoch.
    pub fn add_to_queue(&mut self, raw_epoch: ChainEpoch, values: &BitField) -> anyhow::Result<()> {
        if values.is_empty() {
            // nothing to do.
            return Ok(());
        }

        let epoch: u64 = self.quant.quantize_up(raw_epoch).try_into()?;

        let bitfield = self
            .amt
            .get(epoch)
            .map_err(|e| e.downcast_wrap(format!("failed to lookup queue epoch {}", epoch)))?
            .cloned()
            .unwrap_or_default();

        self.amt
            .set(epoch, &bitfield | values)
            .map_err(|e| e.downcast_wrap(format!("failed to set queue epoch {}", epoch)))?;

        Ok(())
    }

    pub fn add_to_queue_values(&mut self, epoch: ChainEpoch, values: &[u64]) -> anyhow::Result<()> {
        if values.is_empty() {
            Ok(())
        } else {
            self.add_to_queue(epoch, &values.iter().copied().collect())
        }
    }

    /// Cut cuts the elements from the bits in the given bitfield out of the queue,
    /// shifting other bits down and removing any newly empty entries.
    ///
    /// See the docs on `BitField::cut` to better understand what it does.
    pub fn cut(&mut self, to_cut: &BitField) -> anyhow::Result<()> {
        let mut epochs_to_remove = Vec::<u64>::new();

        self.amt
            .for_each_mut(|epoch, bitfield| {
                let bf = bitfield.cut(to_cut);

                if bf.is_empty() {
                    epochs_to_remove.push(epoch);
                } else {
                    **bitfield = bf;
                }

                Ok(())
            })
            .map_err(|e| e.downcast_wrap("failed to cut from bitfield queue"))?;

        self.amt
            .batch_delete(epochs_to_remove, true)
            .map_err(|e| e.downcast_wrap("failed to remove empty epochs from bitfield queue"))?;

        Ok(())
    }

    pub fn add_many_to_queue_values(
        &mut self,
        values: &HashMap<ChainEpoch, Vec<u64>>,
    ) -> anyhow::Result<()> {
        // Update each epoch in-order to be deterministic.
        // Pre-quantize to reduce the number of updates.

        let mut quantized_values = HashMap::<ChainEpoch, Vec<u64>>::with_capacity(values.len());

        for (&raw_epoch, entries) in values {
            let epoch = self.quant.quantize_up(raw_epoch);
            quantized_values.entry(epoch).or_default().extend(entries);
        }

        // Update each epoch in order to be deterministic.
        let mut updated_epochs = Vec::with_capacity(quantized_values.len());
        for epoch in quantized_values.keys() {
            updated_epochs.push(*epoch);
        }
        updated_epochs.sort_unstable();

        for epoch in updated_epochs {
            self.add_to_queue_values(epoch, &quantized_values.remove(&epoch).unwrap_or_default())?;
        }

        Ok(())
    }

    /// Removes and returns all values with keys less than or equal to until.
    /// Modified return value indicates whether this structure has been changed by the call.
    pub fn pop_until(&mut self, until: ChainEpoch) -> anyhow::Result<(BitField, bool)> {
        let mut popped_values = BitField::new();
        let mut popped_keys = Vec::<u64>::new();

        self.amt.for_each_while(|epoch, bitfield| {
            if epoch as ChainEpoch > until {
                // break
                return Ok(false);
            }

            popped_keys.push(epoch);
            popped_values |= bitfield;
            Ok(true)
        })?;

        if popped_keys.is_empty() {
            // Nothing expired.
            return Ok((BitField::new(), false));
        }

        self.amt.batch_delete(popped_keys, true)?;
        Ok((popped_values, true))
    }
}
