#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Trading Pair config is not registered")]
    TradingPairConfigNotFound,
}
