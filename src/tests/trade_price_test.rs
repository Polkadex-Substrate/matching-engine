use crate::Orderbook;
use orderbook_primitives::ocex::TradingPairConfig;
use orderbook_primitives::types::{Order, OrderSide, OrderType, TradingPair};
use polkadex_primitives::AssetId;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::collections::BTreeMap;


// This test was added to check the different price matching reservation bug
#[test]
pub fn test_trade_price() {
    env_logger::init();
    let pair = TradingPair::from(AssetId::Asset(1), AssetId::Polkadex);
    let mut maker_order = Order::random_order_for_testing(pair, OrderSide::Ask, OrderType::LIMIT);
    maker_order.price = Decimal::from_f32(1.0).unwrap();
    maker_order.qty = Decimal::from_f32(10.0).unwrap();

    let mut taker_order = Order::random_order_for_testing(pair, OrderSide::Bid, OrderType::LIMIT);
    taker_order.price = Decimal::from_f32(2.0).unwrap();
    taker_order.qty = Decimal::from_f32(20.0).unwrap();

    let mut orderbook = Orderbook::new();
    orderbook.add_trading_pair(TradingPairConfig::default(pair.base, pair.quote));
    // Add Maker balances
    orderbook.balances.insert(
        (maker_order.main_account.clone(), AssetId::Asset(1)),
        (100.0.try_into().unwrap(), 0.0.try_into().unwrap()),
    );
    orderbook.balances.insert(
        (maker_order.main_account.clone(), AssetId::Polkadex),
        (100.0.try_into().unwrap(), 0.0.try_into().unwrap()),
    );

    // Add taker balances
    orderbook.balances.insert(
        (taker_order.main_account.clone(), AssetId::Asset(1)),
        (100.0.try_into().unwrap(), 0.0.try_into().unwrap()),
    );
    orderbook.balances.insert(
        (taker_order.main_account.clone(), AssetId::Polkadex),
        (100.0.try_into().unwrap(), 0.0.try_into().unwrap()),
    );

    let result = orderbook.process_order(maker_order.clone(), 1).unwrap();
    assert!(result.trades.is_empty());
    assert_eq!(result.stid, 1);
    assert_eq!(
        result.pricelevels,
        BTreeMap::from([(
            (pair, OrderSide::Ask, Decimal::from_f32(1.0).unwrap()),
            Decimal::from_f32(10.0).unwrap()
        )])
    );

    let result = orderbook.process_order(taker_order.clone(), 2).unwrap();
    assert_eq!(result.trades.len(), 1);
    assert_eq!(result.stid, 2);
    assert_eq!(
        result.pricelevels,
        BTreeMap::from([
            (
                (pair, OrderSide::Ask, Decimal::from_f32(1.0).unwrap()),
                Decimal::from_f32(0.0).unwrap()
            ),
            (
                (pair, OrderSide::Bid, Decimal::from_f32(2.0).unwrap()),
                Decimal::from_f32(10.0).unwrap()
            )
        ])
    );
    let trade = result.trades[0].clone();

    assert_eq!(trade.price, maker_order.price);

    assert_eq!(
        orderbook
            .balances
            .get(&(taker_order.main_account.clone(), pair.quote))
            .unwrap()
            .1,
        20.into()
    );
}
