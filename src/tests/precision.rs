use orderbook_primitives::ocex::TradingPairConfig;
use orderbook_primitives::types::{Order, OrderSide, OrderType, TradingPair};
use polkadex_primitives::{AccountId, AssetId};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use crate::Orderbook;

#[test]
pub fn test_order_processing_precision(){
    env_logger::init();
    let pair = TradingPair::from(AssetId::Asset(1), AssetId::Polkadex);
    let mut maker_order = Order::random_order_for_testing(pair, OrderSide::Bid, OrderType::LIMIT);
    maker_order.price = Decimal::from_f32(0.6275).unwrap();
    maker_order.qty = Decimal::from_f32(10.0).unwrap();
    maker_order.main_account = AccountId::new([1;32]);

    let mut taker_order = Order::random_order_for_testing(pair, OrderSide::Ask, OrderType::LIMIT);
    taker_order.price = Decimal::from_f32(0.6275).unwrap();
    taker_order.qty = Decimal::from_f32(1.8).unwrap();
    taker_order.main_account = AccountId::new([2;32]);

    assert_ne!(maker_order.main_account,taker_order.main_account);
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
        (2.41970783.try_into().unwrap(), 0.0.try_into().unwrap()),
    );

    let result = orderbook.process_order(maker_order.clone(), 1).unwrap();
    assert!(result.trades.is_empty());

    let result  = orderbook.process_order(taker_order.clone(),2).unwrap();
    assert_eq!(result.trades.len(),1);

    let (f,_r) =orderbook.balances.get(&(taker_order.main_account.clone(), AssetId::Polkadex)).unwrap().clone();
    assert_eq!(f,0.61970783.try_into().unwrap());

}