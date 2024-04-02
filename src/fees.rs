// This file is part of Polkadex.
//
// Copyright (c) 2023 Polkadex oü.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use frame_support::sp_runtime::traits::AccountIdConversion;
use orderbook_primitives::constants::FEE_POT_PALLET_ID;
use polkadex_primitives::fees::FeeConfig;
use polkadex_primitives::{AccountId, AssetId};
use rust_decimal::{Decimal, RoundingStrategy};
use sp_core::H256;
use std::collections::BTreeMap;

/// A structure that contains the maker and taker fee
/// percentages for the given
#[derive(Copy, Clone, Debug)]
pub struct AccountFee {
    pub maker_fraction: Decimal,
    pub taker_fraction: Decimal,
}

impl Default for AccountFee {
    fn default() -> Self {
        let config = FeeConfig::default();
        Self {
            maker_fraction: config.maker_fraction,
            taker_fraction: config.taker_fraction,
        }
    }
}

/// Fee Receipt
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeReceipt {
    pub user: AccountId, // main account
    pub trade_id: H256,
    pub asset: AssetId,
    pub amt: Decimal,
    pub is_maker: bool,
}

/// Fee collector settles fees for each trade given to it.
/// It will also have a mechanism to withdraw fees too.
pub struct FeeCollector {
    // Main account of fees pot
    pub(crate) pot: AccountId,
    // Accounts to fee structure map
    pub(crate) fee_structure: BTreeMap<AccountId, AccountFee>,
}

impl FeeCollector {
    pub fn initialize() -> Self {
        Self {
            pot: FEE_POT_PALLET_ID.into_account_truncating(),
            fee_structure: Default::default(),
        }
    }

    /// Calculates and returns the fees that must be added/deducted from maker and taker.
    /// NOTE: This method assumes that trade is already settled with NO FEE assumption and the result
    /// of this method is updated on top of that NO FEE SETTLEMENT state, to add fees.
    pub fn settle_trade_fees(
        &mut self,
        main: &AccountId,
        trade_id: H256,
        is_maker: bool,
        recv_amt: &mut Decimal,
        recv_asset: AssetId,
    ) -> FeeReceipt {
        let fee_structure = self.fee_structure.get(main).cloned().unwrap_or_default();

        let fee_fraction = if is_maker {
            fee_structure.maker_fraction
        } else {
            fee_structure.taker_fraction
        };
        // Calculate the fees
        let fees = recv_amt
            .saturating_mul(fee_fraction)
            .round_dp_with_strategy(9, RoundingStrategy::ToZero);
        // Calculate the recv_amt
        *recv_amt = recv_amt
            .saturating_sub(fees)
            .round_dp_with_strategy(9, RoundingStrategy::ToZero);

        // Return receipt
        FeeReceipt {
            user: main.clone(),
            is_maker,
            trade_id,
            asset: recv_asset,
            amt: fees,
        }
    }

    /// Update the fees structure of given account
    pub fn update_fee_structure(
        &mut self,
        main: &AccountId,
        maker_fraction: Decimal,
        taker_fraction: Decimal,
    ) -> AccountFee {
        let fee = self
            .fee_structure
            .entry(main.clone())
            .and_modify(|fee_structure| {
                fee_structure.maker_fraction = maker_fraction;
                fee_structure.taker_fraction = taker_fraction;
            })
            .or_insert(AccountFee {
                maker_fraction,
                taker_fraction,
            });
        *fee
    }
}
