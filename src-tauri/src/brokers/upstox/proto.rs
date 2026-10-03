//! Upstox Market Data Feed V3 messages (`proto/MarketDataFeedV3.proto`,
//! package `com.upstox.marketdatafeederv3udapi.rpc.proto`).
//!
//! Checked in as the code `prost-build` generates for that schema, so a
//! build needs no `protoc`. Field numbers and types follow the `.proto`
//! verbatim; regenerate by hand if Upstox revises it. `LTPC.iep` is a
//! `google.protobuf.DoubleValue` wrapper, modelled by `DoubleValue` here so
//! its presence survives decoding (the web reads presence, not value).

#![allow(clippy::derive_partial_eq_without_eq)]

/// `google.protobuf.DoubleValue`.
#[derive(Clone, Copy, PartialEq, ::prost::Message)]
pub struct DoubleValue {
    #[prost(double, tag = "1")]
    pub value: f64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Ltpc {
    #[prost(double, tag = "1")]
    pub ltp: f64,
    #[prost(int64, tag = "2")]
    pub ltt: i64,
    #[prost(int64, tag = "3")]
    pub ltq: i64,
    #[prost(double, tag = "4")]
    pub cp: f64,
    /// Indicative equilibrium price; set only during pre-open / closing
    /// auction.
    #[prost(message, optional, tag = "5")]
    pub iep: ::core::option::Option<DoubleValue>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MarketLevel {
    #[prost(message, repeated, tag = "1")]
    pub bid_ask_quote: ::prost::alloc::vec::Vec<Quote>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MarketOhlc {
    #[prost(message, repeated, tag = "1")]
    pub ohlc: ::prost::alloc::vec::Vec<Ohlc>,
}

#[derive(Clone, Copy, PartialEq, ::prost::Message)]
pub struct Quote {
    #[prost(int64, tag = "1")]
    pub bid_q: i64,
    #[prost(double, tag = "2")]
    pub bid_p: f64,
    #[prost(int64, tag = "3")]
    pub ask_q: i64,
    #[prost(double, tag = "4")]
    pub ask_p: f64,
}

#[derive(Clone, Copy, PartialEq, ::prost::Message)]
pub struct OptionGreeks {
    #[prost(double, tag = "1")]
    pub delta: f64,
    #[prost(double, tag = "2")]
    pub theta: f64,
    #[prost(double, tag = "3")]
    pub gamma: f64,
    #[prost(double, tag = "4")]
    pub vega: f64,
    #[prost(double, tag = "5")]
    pub rho: f64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Ohlc {
    #[prost(string, tag = "1")]
    pub interval: ::prost::alloc::string::String,
    #[prost(double, tag = "2")]
    pub open: f64,
    #[prost(double, tag = "3")]
    pub high: f64,
    #[prost(double, tag = "4")]
    pub low: f64,
    #[prost(double, tag = "5")]
    pub close: f64,
    #[prost(int64, tag = "6")]
    pub vol: i64,
    #[prost(int64, tag = "7")]
    pub ts: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MarketFullFeed {
    #[prost(message, optional, tag = "1")]
    pub ltpc: ::core::option::Option<Ltpc>,
    #[prost(message, optional, tag = "2")]
    pub market_level: ::core::option::Option<MarketLevel>,
    #[prost(message, optional, tag = "3")]
    pub option_greeks: ::core::option::Option<OptionGreeks>,
    #[prost(message, optional, tag = "4")]
    pub market_ohlc: ::core::option::Option<MarketOhlc>,
    /// Average traded price.
    #[prost(double, tag = "5")]
    pub atp: f64,
    /// Volume traded today.
    #[prost(int64, tag = "6")]
    pub vtt: i64,
    #[prost(double, tag = "7")]
    pub oi: f64,
    #[prost(double, tag = "8")]
    pub iv: f64,
    #[prost(double, tag = "9")]
    pub tbq: f64,
    #[prost(double, tag = "10")]
    pub tsq: f64,
    #[prost(double, tag = "11")]
    pub iep: f64,
    #[prost(double, tag = "12")]
    pub rp: f64,
    #[prost(int64, tag = "13")]
    pub ieq: i64,
    /// Net unmatched quantity at the IEP; signed.
    #[prost(int64, tag = "14")]
    pub iiq_total: i64,
    #[prost(int64, tag = "15")]
    pub iiq_m: i64,
    #[prost(bool, tag = "16")]
    pub cas_eligible: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct IndexFullFeed {
    #[prost(message, optional, tag = "1")]
    pub ltpc: ::core::option::Option<Ltpc>,
    #[prost(message, optional, tag = "2")]
    pub market_ohlc: ::core::option::Option<MarketOhlc>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FullFeed {
    #[prost(oneof = "full_feed::FullFeedUnion", tags = "1, 2")]
    pub full_feed_union: ::core::option::Option<full_feed::FullFeedUnion>,
}

pub mod full_feed {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum FullFeedUnion {
        #[prost(message, tag = "1")]
        MarketFf(super::MarketFullFeed),
        #[prost(message, tag = "2")]
        IndexFf(super::IndexFullFeed),
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FirstLevelWithGreeks {
    #[prost(message, optional, tag = "1")]
    pub ltpc: ::core::option::Option<Ltpc>,
    #[prost(message, optional, tag = "2")]
    pub first_depth: ::core::option::Option<Quote>,
    #[prost(message, optional, tag = "3")]
    pub option_greeks: ::core::option::Option<OptionGreeks>,
    #[prost(int64, tag = "4")]
    pub vtt: i64,
    #[prost(double, tag = "5")]
    pub oi: f64,
    #[prost(double, tag = "6")]
    pub iv: f64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Feed {
    #[prost(oneof = "feed::FeedUnion", tags = "1, 2, 3")]
    pub feed_union: ::core::option::Option<feed::FeedUnion>,
    #[prost(enumeration = "RequestMode", tag = "4")]
    pub request_mode: i32,
}

pub mod feed {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum FeedUnion {
        #[prost(message, tag = "1")]
        Ltpc(super::Ltpc),
        #[prost(message, tag = "2")]
        FullFeed(super::FullFeed),
        #[prost(message, tag = "3")]
        FirstLevelWithGreeks(super::FirstLevelWithGreeks),
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StatusInfo {
    #[prost(string, tag = "1")]
    pub status: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub updated_time: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MarketInfo {
    #[prost(map = "string, enumeration(MarketStatus)", tag = "1")]
    pub segment_status: ::std::collections::HashMap<::prost::alloc::string::String, i32>,
    #[prost(map = "string, message", tag = "2")]
    pub cas_market_status: ::std::collections::HashMap<::prost::alloc::string::String, StatusInfo>,
    #[prost(map = "string, message", tag = "3")]
    pub pre_open_session_status:
        ::std::collections::HashMap<::prost::alloc::string::String, StatusInfo>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FeedResponse {
    #[prost(enumeration = "Type", tag = "1")]
    pub r#type: i32,
    #[prost(map = "string, message", tag = "2")]
    pub feeds: ::std::collections::HashMap<::prost::alloc::string::String, Feed>,
    #[prost(int64, tag = "3")]
    pub current_ts: i64,
    #[prost(message, optional, tag = "4")]
    pub market_info: ::core::option::Option<MarketInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum Type {
    InitialFeed = 0,
    LiveFeed = 1,
    MarketInfo = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum RequestMode {
    Ltpc = 0,
    FullD5 = 1,
    OptionGreeks = 2,
    FullD30 = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum MarketStatus {
    PreOpenStart = 0,
    PreOpenEnd = 1,
    NormalOpen = 2,
    NormalClose = 3,
    ClosingStart = 4,
    ClosingEnd = 5,
}
