use orderbook_primitives::types::{Order, OrderSide, OrderStatus, OrderType, Trade, TradingPair};
use polkadex_primitives::AssetId;
use rust_decimal::prelude::Zero;
use rust_decimal::Decimal;

/// Calculate the amount of assets that will be received and given away when a trade settles
/// # Arguments
/// * `price` - Price of the asset
/// * `side` - Side of the order
/// * `pair` - Trading pair
/// * `amount` - Amount of asset
/// # Returns
/// * `AssetId` - Asset that will be received
/// * `Decimal` - Amount of asset that will be received
/// * `AssetId` - Asset that will be given away
/// * `Decimal` - Amount of asset that will be given away
/// # Example
/// consider market BTC/USD and a user places a bid order for 1 BTC at price 500 USD and the order is matched
/// then the user receiving_asset will be BTC and recv_amt will be 1 BTC and give_away_asset will be USD and amt_lost will be 500 USD
pub fn calculate_assets_flows_from_trade(
    price: Decimal,
    side: OrderSide,
    pair: TradingPair,
    amount: Decimal,
) -> (AssetId, Decimal, AssetId, Decimal) {
    // receiving_asset, recv_amt, give_away_asset, amt_lost
    let quote_flow = Order::rounding_off(price.saturating_mul(amount));
    match side {
        // Asker will get quote and lose base asset when trade settles
        OrderSide::Ask => (pair.quote, quote_flow, pair.base, amount),
        // Bidder will get base and lose quote asset when trade settles
        OrderSide::Bid => (pair.base, amount, pair.quote, quote_flow),
    }
}

/// Checks if there is enough unreserved balance for closing limit orders in trades
///
/// # Parameters
/// * `order`:  a reference to an Order object
/// * `min_volume`: minimum volume allowed for the trading pair
///
/// # Returns
/// * `Decimal`: Returns un reserve locked balance that needs
/// to be unlocked for closing limit orders in trades
pub fn check_unreserved_balance_for_close_limit_orders_in_trades(
    order: &Order,
    min_volume: Decimal,
) -> Decimal {
    let amount = if order.side == OrderSide::Ask {
        order.qty.saturating_sub(order.filled_quantity)
    } else {
        order
            .qty
            .saturating_sub(order.filled_quantity)
            .saturating_mul(order.price)
    };

    if (order.order_type == OrderType::LIMIT && amount != Decimal::zero())
        && (order.status == OrderStatus::CLOSED || order.available_volume(None) < min_volume)
    {
        return amount;
    }
    Decimal::zero()
}

// check if orders can be matched
// if taker is market order, it can be matched with any price will always return true.
// if taker is limit order, it can be matched with maker if maker price is better than taker price
pub fn will_orders_match(taker: &Order, maker: &Order) -> bool {
    if taker.order_type == OrderType::MARKET {
        return true;
    }
    match taker.side {
        OrderSide::Ask => taker.price.le(&maker.price),
        OrderSide::Bid => maker.price.le(&taker.price),
    }
}

// match two orders and return trade
pub fn execute(taker: &mut Order, maker: &mut Order, qty_step_size: Decimal) -> Option<Trade> {
    let price = maker.price;

    let mut quantity_available = match (taker.side, taker.order_type) {
        (OrderSide::Bid, OrderType::MARKET) => {
            // If Market order is defined in base quantity
            if !taker.qty.is_zero() {
                taker.qty.saturating_sub(taker.filled_quantity)
            } else {
                // Get quote required and divide it by current price to get needed_base
                let mut available_qty = Order::rounding_off(
                    taker
                        .available_volume(Some(maker.price))
                        .checked_div(price)
                        .unwrap_or_else(Decimal::zero),
                );
                // Convert it into a multiple of qty_step_size
                available_qty = Order::rounding_off(
                    available_qty
                        .checked_div(qty_step_size)
                        .unwrap_or_else(Decimal::zero)
                        .saturating_mul(qty_step_size),
                );
                // If available_quantity is zero don't execute the trade0
                if available_qty.is_zero() {
                    return None;
                }
                available_qty
            }
        }
        (_, _) => Order::rounding_off(Order::rounding_off(
            taker.qty.saturating_sub(taker.filled_quantity),
        )),
    };

    let maker_available_qty = Order::rounding_off(maker.qty.saturating_sub(maker.filled_quantity));
    if maker_available_qty.le(&quantity_available) {
        if maker_available_qty.eq(&quantity_available) {
            // If maker and taker have equal qty to fill, then both will be closed.
            taker.status = OrderStatus::CLOSED
        }
        quantity_available = maker_available_qty;
        // Maker is smaller than taker so it will be closed.
        maker.status = OrderStatus::CLOSED;
    }

    taker.update_avg_price_and_filled_qty(price, quantity_available);
    maker.update_avg_price_and_filled_qty(price, quantity_available);

    Some(Trade::new(
        maker.clone(),
        taker.clone(),
        price,
        quantity_available,
    ))
}
