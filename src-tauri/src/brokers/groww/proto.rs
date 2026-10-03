//! Groww live-data protobuf messages, checked in (no protoc at build
//! time). Field numbers and types follow the web's hand decoder
//! (`streaming/groww_protobuf.py`); every price is a little-endian double
//! (`fixed64`). Written in the shape `prost-build` generates.

/// Outer message of every NATS payload.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LiveData {
    #[prost(string, tag = "1")]
    pub symbol: ::prost::alloc::string::String,
    /// `0 CASH, 1 FNO, 2 CURRENCY, 3 COMMODITY`.
    #[prost(int32, tag = "2")]
    pub segment: i32,
    /// `0 BSE, 1 NSE, 2 MCX, 3 MCXSX, 4 NCDEX, 5 GLOBAL, 6 US`.
    #[prost(int32, tag = "3")]
    pub exchange: i32,
    #[prost(message, optional, tag = "4")]
    pub ltp_data: ::core::option::Option<StocksLivePrice>,
    #[prost(message, optional, tag = "5")]
    pub depth_data: ::core::option::Option<MarketDepth>,
    #[prost(message, optional, tag = "6")]
    pub index_data: ::core::option::Option<LiveIndex>,
}

/// `StocksLivePriceProto` (fields 8-12 are not read by the web).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StocksLivePrice {
    #[prost(double, tag = "1")]
    pub ts_in_millis: f64,
    #[prost(double, tag = "2")]
    pub open: f64,
    #[prost(double, tag = "3")]
    pub high: f64,
    #[prost(double, tag = "4")]
    pub low: f64,
    #[prost(double, tag = "5")]
    pub close: f64,
    #[prost(double, tag = "6")]
    pub volume: f64,
    #[prost(double, tag = "7")]
    pub value: f64,
    #[prost(double, tag = "13")]
    pub ltp: f64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MarketDepth {
    #[prost(double, tag = "1")]
    pub ts_in_millis: f64,
    #[prost(message, repeated, tag = "2")]
    pub buy: ::prost::alloc::vec::Vec<DepthLevel>,
    #[prost(message, repeated, tag = "3")]
    pub sell: ::prost::alloc::vec::Vec<DepthLevel>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DepthLevel {
    #[prost(int64, tag = "1")]
    pub orders: i64,
    #[prost(message, optional, tag = "2")]
    pub price_qty: ::core::option::Option<PriceQty>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PriceQty {
    #[prost(double, tag = "1")]
    pub price: f64,
    #[prost(double, tag = "2")]
    pub quantity: f64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LiveIndex {
    #[prost(double, tag = "1")]
    pub ts_in_millis: f64,
    #[prost(double, tag = "2")]
    pub value: f64,
}

/// Decode one payload; `None` when it is not a Groww live-data message.
pub fn decode(bytes: &[u8]) -> Option<LiveData> {
    <LiveData as prost::Message>::decode(bytes).ok()
}
