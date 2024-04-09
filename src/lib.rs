mod error;
mod fees;
mod utils;

#[cfg(test)]
mod tests;

use crate::error::Error;
use crate::fees::{AccountFee, FeeCollector};
use crate::utils::{
    calculate_assets_flows_from_trade, check_unreserved_balance_for_close_limit_orders_in_trades,
    execute, will_orders_match,
};
use anyhow::anyhow;
use log::info;
use orderbook_primitives::ocex::TradingPairConfig;
use orderbook_primitives::types::{
    Order, OrderId, OrderSide, OrderStatus, OrderType, Trade, TradingPair,
};
use polkadex_primitives::{AccountId, AssetId};
use rust_decimal::prelude::Zero;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BinaryHeap};

/// (TradingPair, OrderSide, Price) => Amount
pub type PriceLevels = BTreeMap<(TradingPair, OrderSide, Decimal), Decimal>;

#[derive(Default, Debug)]
pub struct OrderExecutionResult {
    // Final state of balances (main, assetid ) => (free, reserved)
    balances: BTreeMap<(AccountId, AssetId), (Decimal, Decimal)>,
    // Final Price level state
    pricelevels: PriceLevels,
    // Final Order state
    modified_orders: BTreeMap<OrderId, Order>,
    // Trades generated
    trades: Vec<Trade>,
    // State change id
    stid: u64,
}

impl OrderExecutionResult {
    pub fn new(stid: u64) -> Self {
        Self {
            balances: Default::default(),
            pricelevels: Default::default(),
            modified_orders: Default::default(),
            trades: vec![],
            stid,
        }
    }
}

pub struct Orderbook {
    // Available trading pairs
    trading_pairs: BTreeMap<TradingPair, TradingPairConfig>,
    // Keeps track of price levels and corresponding cummulative amounts
    pricelevels: PriceLevels,
    // In-memory cache of Bid Orderbooks
    bid_books: BTreeMap<TradingPair, BinaryHeap<Order>>,
    // In-memory cache of Ask Orderbooks
    ask_books: BTreeMap<TradingPair, BinaryHeap<Order>>,
    // Final state of balances (main, assetid ) => (free, reserved)
    balances: BTreeMap<(AccountId, AssetId), (Decimal, Decimal)>,
    // Fee Collector
    fees_collector: FeeCollector,
}

impl Default for Orderbook {
    fn default() -> Self {
        Self::new()
    }
}

impl Orderbook {
    pub fn new() -> Self {
        Self {
            trading_pairs: Default::default(),
            pricelevels: Default::default(),
            bid_books: Default::default(),
            ask_books: Default::default(),
            balances: Default::default(),
            fees_collector: FeeCollector::initialize(),
        }
    }

    pub fn load(
        trading_pairs: BTreeMap<TradingPair, TradingPairConfig>,
        bid_books: BTreeMap<TradingPair, BinaryHeap<Order>>,
        ask_books: BTreeMap<TradingPair, BinaryHeap<Order>>,
        balances: BTreeMap<(AccountId, AssetId), (Decimal, Decimal)>,
        fee_structures: BTreeMap<AccountId, AccountFee>,
    ) -> Self {
        let mut fees_collector = FeeCollector::initialize();
        fees_collector.fee_structure = fee_structures;
        Self {
            trading_pairs,
            pricelevels: Default::default(),
            bid_books,
            ask_books,
            balances,
            fees_collector,
        }
    }

    pub fn update_fee_structure(
        &mut self,
        main: &AccountId,
        maker_fraction: Decimal,
        taker_fraction: Decimal,
    ) {
        self.fees_collector
            .update_fee_structure(main, maker_fraction, taker_fraction);
    }

    // This function will get the market config for the given pair.
    // If the pair is not found in the config, it will return the default config.
    pub fn get_pair_config(&self, pair: &TradingPair) -> Option<TradingPairConfig> {
        let config = self.trading_pairs.get(pair).cloned();
        config
    }

    // Check if the order can match
    pub fn will_match(&self, order: &Order) -> bool {
        if order.order_type == OrderType::MARKET {
            return true;
        }
        let book = match order.side {
            OrderSide::Ask => self.bid_books.get(&order.pair),
            OrderSide::Bid => self.ask_books.get(&order.pair),
        };
        if let Some(book) = book {
            if let Some(open_order) = book.peek() {
                return match order.side {
                    OrderSide::Ask => order.price <= open_order.price,
                    OrderSide::Bid => order.price >= open_order.price,
                };
            }
        }
        false
    }

    pub fn match_order(
        &mut self,
        config: &TradingPairConfig,
        taker: &mut Order,
        trade_changes: &mut Vec<Trade>,
    ) {
        match taker.order_type {
            OrderType::LIMIT => self.match_limit(taker, trade_changes, config),
            OrderType::MARKET => self.match_market(taker, trade_changes, config),
        }
    }

    // This function will match the order with the opposite side of the book.
    // If the order is not fully filled, it will insert the order into the book.
    pub fn match_limit(
        &mut self,
        taker: &mut Order,
        trade_changes: &mut Vec<Trade>,
        config: &TradingPairConfig,
    ) {
        self.match_side(taker, trade_changes, config);
        // close the order if the available volume to trade is less than min config for the market
        if taker.available_volume(None).lt(&config.min_volume()) {
            taker.status = OrderStatus::CLOSED;
        }
    }

    // This function will match the order with the opposite side of the book
    // and
    // add the changes to the StateChanges cache.
    // closes the order regardless of whether it is fully filled
    // or not as market orders cannot stay open
    pub fn match_market(
        &mut self,
        taker: &mut Order,
        trade_changes: &mut Vec<Trade>,
        config: &TradingPairConfig,
    ) {
        self.match_side(taker, trade_changes, config);
        //close the order as market orders cannot stay open
        taker.status = OrderStatus::CLOSED;
        self.change_status_of_order_in_trade(trade_changes);
    }

    pub fn change_status_of_order_in_trade(&self, trade_changes: &mut [Trade]) {
        let last_index = trade_changes.len().saturating_sub(1);
        if let Some(last_trade) = trade_changes.get_mut(last_index) {
            last_trade.taker.status = OrderStatus::CLOSED;
        }
    }

    pub fn settle_order_updates(
        &mut self,
        order: &Order,
        changes: &mut OrderExecutionResult,
    ) -> anyhow::Result<()> {
        // If the order is still open, insert it into the orderbook.
        if order.status == OrderStatus::OPEN {
            self.insert_order(order)?;
        }
        //add current order to the orderbook
        changes.modified_orders.insert(order.id, order.clone());

        //go through the trades and update the modified orders
        for trade in &changes.trades {
            //update the maker order
            let mut maker = trade.maker.clone();
            maker.stid = changes.stid;
            changes.modified_orders.insert(maker.id, maker);
        }
        Ok(())
    }

    pub fn insert_order(&mut self, order: &Order) -> anyhow::Result<()> {
        let book = match order.side {
            OrderSide::Ask => self.ask_books.get_mut(&order.pair),
            OrderSide::Bid => self.bid_books.get_mut(&order.pair),
        };
        //add to the orderbook
        if let Some(item) = book {
            item.push(order.clone());
            Ok(())
        } else {
            Err(anyhow!(anyhow::Error::msg("order book not opened")))
        }
    }

    pub fn settle_price_level_updates(
        &mut self,
        config: &TradingPairConfig,
        order: &Order,
        changes: &mut OrderExecutionResult,
    ) {
        //add the current order to the price level if its still open
        //NOTE: as we are using saturating sub, we the order here is important. First add to price level then subtract.
        if order.status == OrderStatus::OPEN {
            let unfilled = order
                .qty
                .saturating_sub(order.filled_quantity)
                .max(Decimal::zero());
            self.add_to_pricelevel(
                config,
                order.pair,
                order.price,
                unfilled,
                order.side,
                &mut changes.pricelevels,
            );
        }
        //loop through trades generated and create price level updates
        for trade in &changes.trades {
            // update the price levels. The price levels are updated for the maker side.
            self.reduce_from_pricelevel(
                config,
                trade.maker.pair,
                trade.price,
                trade.amount,
                trade.maker.side,
                &mut changes.pricelevels,
            );
        }
    }

    pub fn add_to_pricelevel(
        &mut self,
        config: &TradingPairConfig,
        pair: TradingPair,
        price: Decimal,
        qty: Decimal,
        side: OrderSide,
        pricelevel_changes: &mut PriceLevels,
    ) {
        let mut q = *self
            .pricelevels
            .entry((pair, side, price))
            .and_modify(|q| {
                *q = q.saturating_add(qty).max(Decimal::zero());
            })
            .or_insert(qty);
        let vol = price.saturating_mul(q);
        q = vol
            .lt(&config.min_volume())
            .then(Decimal::zero)
            .unwrap_or(q);
        // the price level q zero we remove it.
        if q.is_zero() {
            self.pricelevels.remove(&(pair, side, price));
        }
        // Add it to price level changes for publishing
        pricelevel_changes.insert((pair, side, price), q);
        log::info!(target:"engine","Update (Inc) price level: {:?} - {:?} - {:?}: qty: {:?}",pair,side,price,q);
    }

    pub fn reduce_from_pricelevel(
        &mut self,
        config: &TradingPairConfig,
        pair: TradingPair,
        price: Decimal,
        qty: Decimal,
        side: OrderSide,
        pricelevel_changes: &mut PriceLevels,
    ) {
        let mut q = *self
            .pricelevels
            .entry((pair, side, price))
            .and_modify(|q| {
                *q = q.saturating_sub(qty).max(Decimal::zero());
            })
            .or_insert(qty);

        let vol = price.saturating_mul(q);
        q = vol
            .lt(&config.min_volume())
            .then(Decimal::zero)
            .unwrap_or(q);
        // the price level s zero we remove it.
        if q.is_zero() {
            self.pricelevels.remove(&(pair, side, price));
        }
        // Add it to pricelevel changes for publishing
        pricelevel_changes.insert((pair, side, price), q);
        log::info!(target:"engine","Update (Dec) price level: {:?} - {:?} - {:?}: qty: {:?}",pair,side,price,q);
    }

    /// Updates the fees for order in memory
    pub fn update_in_memory_order_state_with_fee(&mut self, order: &Order) {
        let book_option = match order.side {
            OrderSide::Ask => self.ask_books.get_mut(&order.pair),
            OrderSide::Bid => self.bid_books.get_mut(&order.pair),
        };

        if let Some(book) = book_option {
            println!("book len: {:?}", book.len());
            let mut new_book = BinaryHeap::new();

            while let Some(mut stored_order) = book.pop() {
                if stored_order.id == order.id {
                    stored_order.fee = order.fee;
                    new_book.push(stored_order);
                    break;
                }
                new_book.push(stored_order);
            }

            book.append(&mut new_book);
            println!("book len: {:?}", book.len());
        }
    }

    pub fn settle_trades(
        &mut self,
        trading_pair_config: TradingPairConfig,
        changes: &mut OrderExecutionResult,
    ) {
        info!(target:"engine", "setting {:?} trades", changes.trades.len());
        // We only need to settle trades right now.
        for trade in &mut changes.trades {
            let trade_id = trade.trade_id();
            let Trade {
                maker,
                taker,
                price,
                amount,
                ..
            } = trade;

            // Check if underpriced execution
            debug_assert!((*price).eq(&maker.price));
            match taker.side {
                OrderSide::Ask => {
                    // Ignore - reservation happens in qty so price is not affecting it
                }
                OrderSide::Bid => {
                    if *price < taker.price {
                        let diff = taker.price.saturating_sub(*price);
                        let to_unreserve = diff.saturating_mul(*amount);
                        let final_state = self
                            .balances
                            .entry((taker.main_account.clone(), taker.pair.quote))
                            .and_modify(|(free, reserved)| {
                                *reserved =
                                    reserved.saturating_sub(to_unreserve).max(Decimal::zero());
                                *free = Order::rounding_off(free.saturating_add(to_unreserve));
                            })
                            .or_insert((Decimal::zero(), Decimal::zero()));
                        changes
                            .balances
                            .insert((taker.main_account.clone(), taker.pair.quote), *final_state);
                    }
                }
            }

            let maker_main = maker.main_account.clone();
            let quantity = amount;
            for order in [maker, taker] {
                let min_volume = trading_pair_config.min_volume;

                // Calculate asset flow
                let (receiving_asset, mut recv_amt, give_away_asset, lost_amt) =
                    calculate_assets_flows_from_trade(*price, order.side, order.pair, *quantity);
                info!(target:"engine",
                    "receiving asset: {:?}, recv_amt: {:?}, give_away: {:?}, lost_amt: {:?}",
                    receiving_asset, recv_amt, give_away_asset, lost_amt
                );
                let un_reserve_balance =
                    check_unreserved_balance_for_close_limit_orders_in_trades(order, min_volume);

                let is_maker = order.main_account == maker_main;

                // Collect fees
                let receipt = self.fees_collector.settle_trade_fees(
                    &order.main_account,
                    trade_id,
                    is_maker,
                    &mut recv_amt,
                    receiving_asset,
                );

                // Update the collect fees in the order, note this is cumulative fees.
                order.fee = Order::rounding_off(order.fee.saturating_add(receipt.amt));
                changes.modified_orders.entry(order.id).and_modify(|o| {
                    o.fee = order.fee;
                });

                self.update_in_memory_order_state_with_fee(order);
                // Add fees to fees account
                let final_state = self
                    .balances
                    .entry((self.fees_collector.pot.clone(), receipt.asset))
                    .and_modify(|(free, _)| {
                        *free = Order::rounding_off(free.saturating_add(receipt.amt));
                    })
                    .or_insert((receipt.amt, Decimal::zero()));

                // Apply the final state of fees account to changes cache
                changes.balances.insert(
                    (self.fees_collector.pot.clone(), receipt.asset),
                    *final_state,
                );

                // Reduce the give_away_asset balance of the user by the lost_amt
                let final_state = self
                    .balances
                    .entry((order.main_account.clone(), give_away_asset))
                    .and_modify(|(free, reserved)| {
                        *reserved =
                            reserved.saturating_sub(lost_amt.saturating_add(un_reserve_balance));
                        *free = Order::rounding_off(free.saturating_add(un_reserve_balance));
                    })
                    .or_insert((Decimal::zero(), Decimal::zero()));

                // Apply the final state to changes cache
                changes
                    .balances
                    .insert((order.main_account.clone(), give_away_asset), *final_state);
                info!(target:"engine",
                    "giveaway asset: {:?}, final state: {:?}",
                    give_away_asset, final_state
                );

                // Increase the receiving_asset balance of the user by the recv_amt
                let final_state = self
                    .balances
                    .entry((order.main_account.clone(), receiving_asset))
                    .and_modify(|(free, _reserved)| {
                        *free = Order::rounding_off(free.saturating_add(recv_amt))
                    })
                    .or_insert((recv_amt, Decimal::zero()));

                // Apply the final state to changes cache
                changes
                    .balances
                    .insert((order.main_account.clone(), receiving_asset), *final_state);

                info!(target:"engine",
                    "receiving asset: {:?}, final state: {:?}",
                    receiving_asset, final_state
                );
            }
        }
    }

    pub fn free_reserve_balance_of_market_order(
        &mut self,
        order: &Order,
        changes: &mut OrderExecutionResult,
    ) -> anyhow::Result<()> {
        //Market Order will never get inserted in order-book hence we can unreserve the balances
        if order.order_type == OrderType::MARKET {
            // Handle the unprocessed part of market order
            let (unfilled_amount, asset) = match order.side {
                OrderSide::Ask => {
                    let unfilled_amount = order.qty.saturating_sub(order.filled_quantity);
                    (unfilled_amount, order.pair.base)
                }
                OrderSide::Bid => {
                    let unfilled_volume = order.quote_order_qty.saturating_sub(
                        order.avg_filled_price.saturating_mul(order.filled_quantity),
                    );
                    (unfilled_volume, order.pair.quote)
                }
            };
            if !unfilled_amount.is_zero() {
                self.unreserve_balance(unfilled_amount, asset, order.main_account.clone(), changes);
                log::info!(target:"engine","Un-reserving unfilled balance for market order: {:?}",unfilled_amount);
                return Ok(());
            }
        }
        Ok(())
    }

    // Updates the balance map
    pub fn reserve_balances(
        &mut self,
        order: &Order,
        changes: &mut OrderExecutionResult,
    ) -> anyhow::Result<()> {
        let (asset, amount) = match (order.side, order.order_type) {
            (OrderSide::Bid, OrderType::LIMIT) => (order.pair.quote, order.available_volume(None)),
            (OrderSide::Ask, OrderType::LIMIT) | (OrderSide::Ask, OrderType::MARKET) => (
                order.pair.base,
                order.qty.saturating_sub(order.filled_quantity),
            ),
            (OrderSide::Bid, OrderType::MARKET) => {
                if order.quote_order_qty.is_zero() {
                    (
                        order.pair.base,
                        order.qty.saturating_sub(order.filled_quantity),
                    )
                } else {
                    (order.pair.quote, order.quote_order_qty)
                }
            }
        };
        log::debug!(target: "matching","Reserving {:?} of {:?}", asset,amount);
        let amount = Order::rounding_off(amount);
        let mut is_success = false;
        let final_state = self
            .balances
            .entry((order.main_account.clone(), asset))
            .and_modify(|(free, reserved)| {
                if *free >= amount {
                    *free = free.saturating_sub(amount);
                    *reserved = reserved.saturating_add(amount);
                    is_success = true;
                } else {
                    log::error!(target:"engine","Balance is corrupted: free: {:?},\
                     amount: {:?}, asset: {:?}, main: {:?} ",free,amount,asset,order.main_account)
                }
            })
            .or_insert((Decimal::zero(), Decimal::zero()));
        if is_success {
            changes
                .balances
                .insert((order.main_account.clone(), asset), *final_state);
            return Ok(());
        }
        Err(anyhow::Error::msg("Error while reserving assets for order"))
    }

    pub fn unreserve_balance(
        &mut self,
        amount: Decimal,
        asset: AssetId,
        main: AccountId,
        changes: &mut OrderExecutionResult,
    ) {
        let final_state = self
            .balances
            .entry((main.clone(), asset))
            .and_modify(|(free, reserved)| {
                *reserved = reserved.saturating_sub(amount).max(Decimal::zero());
                *free = Order::rounding_off(free.saturating_add(amount));
            })
            .or_insert((Decimal::zero(), Decimal::zero()));
        changes.balances.insert((main, asset), *final_state);
    }

    // match two orders and add the trade to the changes and modified orders to the StateChanges
    pub fn match_side(
        &mut self,
        taker: &mut Order,
        trade_changes: &mut Vec<Trade>,
        config: &TradingPairConfig,
    ) {
        let start = std::time::Instant::now();
        let mut trades = Vec::new();
        let mut default = BinaryHeap::new();

        let book = match taker.side {
            OrderSide::Ask => self.bid_books.get_mut(&taker.pair).unwrap_or(&mut default),
            OrderSide::Bid => self.ask_books.get_mut(&taker.pair).unwrap_or(&mut default),
        };

        // Consume until the cache is empty
        while !book.is_empty() {
            // Get the first(best) order from the book
            if let Some(mut other) = book.pop() {
                //if takers volume is less than the min volume for the market,
                // close the taker order and push the other order back into the book

                if taker
                    .available_volume(Some(other.price))
                    .lt(&config.min_volume())
                {
                    taker.status = OrderStatus::CLOSED;
                    book.push(other);
                    break;
                }

                if !will_orders_match(taker, &other) {
                    // other is added back into the book
                    book.push(other);
                    break;
                }

                if let Some(mut trade) = execute(taker, &mut other, config.qty_step_size) {
                    if trade
                        .maker
                        .available_volume(Some(other.price))
                        .lt(&config.min_volume())
                    {
                        // We will be dropping the maker order below if this condition is true
                        //why is maker not being removed from the heap ?
                        trade.maker.status = OrderStatus::CLOSED
                    }

                    // Check if other has enough volume to save it back to queue otherwise close it
                    if !other.available_volume(None).lt(&config.min_volume()) {
                        book.push(other.clone());
                        println!(
                            "Other available volume true: {:?}, len: {:?}",
                            other.available_volume(None),
                            book.len()
                        );
                    } else {
                        println!(
                            "Other available volume false: {:?}",
                            other.available_volume(None)
                        );
                        other.status = OrderStatus::CLOSED
                    }
                    trades.push(trade);
                } else {
                    // Other is not changed here so no need to update state change
                    book.push(other);
                    break;
                }
            }
        }
        info!(
            "Matched limit order: {:?} and generated {:?} trades",
            taker.id,
            trades.len()
        );
        info!(target:"engine","[fn:match_side] took {:?}",start.elapsed());
        trade_changes.append(&mut trades);
        println!("Book len: {:?}", book.len());
    }

    pub fn add_trading_pair(&mut self, config: TradingPairConfig) {
        let pair = TradingPair::from(config.quote_asset, config.base_asset);
        self.trading_pairs.insert(pair, config);
        self.bid_books.insert(pair, Default::default());
        self.ask_books.insert(pair, Default::default());
    }

    pub fn process_order(
        &mut self,
        mut order: Order,
        stid: u64,
    ) -> anyhow::Result<OrderExecutionResult> {
        let start = std::time::Instant::now();
        log::info!("Starting to process order {order:?}");
        // Get the pair config if present otherwise return error.
        let config = self
            .get_pair_config(&order.pair)
            .ok_or(Error::TradingPairConfigNotFound)?;

        let mut execution_result = OrderExecutionResult::new(stid);

        // Reserve balances
        self.reserve_balances(&order, &mut execution_result)?;
        log::info!("checking if match can happen");
        if self.will_match(&order) {
            // Order cannot match so insert.
            self.match_order(&config, &mut order, &mut execution_result.trades);
        }
        log::info!("generated {:?} trades", execution_result.trades.len());
        // settle order updates from trades
        self.settle_order_updates(&order, &mut execution_result)?;
        //Settle all price level updates from trades
        self.settle_price_level_updates(&config, &order, &mut execution_result);
        // Settle all balances from trades
        self.settle_trades(config, &mut execution_result);
        // free reserve balance for market order
        self.free_reserve_balance_of_market_order(&order, &mut execution_result)?;
        info!(target:"engine","[fn:process_order] took {:?}", start.elapsed());
        Ok(execution_result)
    }
}
