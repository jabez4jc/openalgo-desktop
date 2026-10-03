//! OpenAlgo order constants (`docs/prompt/order-constants.md`).
//!
//! Every enum parses and prints exactly the string the web uses on the wire,
//! so a value read from an `/api/v1` body round-trips unchanged. Parsing is
//! case-sensitive like the web's marshmallow `OneOf` validators: `nse` is not
//! an exchange.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// A value that is not one of the documented constants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidConstant {
    pub kind: &'static str,
    pub value: String,
}

impl fmt::Display for InvalidConstant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid {} '{}'", self.kind, self.value)
    }
}

impl std::error::Error for InvalidConstant {}

macro_rules! oa_enum {
    (
        $(#[$meta:meta])*
        $name:ident, $kind:literal, { $($variant:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every value, in documentation order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The exact web string.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = InvalidConstant;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok($name::$variant),)+
                    _ => Err(InvalidConstant { kind: $kind, value: s.to_string() }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

oa_enum!(
    /// OpenAlgo exchange codes.
    Exchange, "exchange", {
        Nse => "NSE",
        Nfo => "NFO",
        Cds => "CDS",
        Bse => "BSE",
        Bfo => "BFO",
        Bcd => "BCD",
        Mcx => "MCX",
        Ncdex => "NCDEX",
        Nco => "NCO",
        NseIndex => "NSE_INDEX",
        BseIndex => "BSE_INDEX",
        McxIndex => "MCX_INDEX",
        GlobalIndex => "GLOBAL_INDEX",
        Crypto => "CRYPTO",
    }
);

impl Exchange {
    /// Quote-only index exchanges.
    pub fn is_index(self) -> bool {
        matches!(
            self,
            Exchange::NseIndex | Exchange::BseIndex | Exchange::McxIndex | Exchange::GlobalIndex
        )
    }

    /// Derivatives segments (the ones that carry open interest).
    pub fn is_derivative(self) -> bool {
        matches!(
            self,
            Exchange::Nfo
                | Exchange::Bfo
                | Exchange::Cds
                | Exchange::Bcd
                | Exchange::Mcx
                | Exchange::Nco
                | Exchange::Ncdex
                | Exchange::Crypto
        )
    }

    /// Cash segments, where the carry product is `CNC`.
    pub fn is_cash(self) -> bool {
        matches!(self, Exchange::Nse | Exchange::Bse)
    }
}

oa_enum!(
    /// OpenAlgo product types.
    Product, "product", {
        Cnc => "CNC",
        Nrml => "NRML",
        Mis => "MIS",
    }
);

oa_enum!(
    /// OpenAlgo price types.
    PriceType, "pricetype", {
        Market => "MARKET",
        Limit => "LIMIT",
        Sl => "SL",
        SlM => "SL-M",
    }
);

oa_enum!(
    /// Order side.
    Action, "action", {
        Buy => "BUY",
        Sell => "SELL",
    }
);

impl Action {
    pub fn opposite(self) -> Action {
        match self {
            Action::Buy => Action::Sell,
            Action::Sell => Action::Buy,
        }
    }
}

oa_enum!(
    /// Order validity. The web forces `DAY` for most brokers.
    Validity, "validity", {
        Day => "DAY",
        Ioc => "IOC",
    }
);

oa_enum!(
    /// Normalised order status: the lowercase vocabulary the web's
    /// `transform_order_data` emits in `order_status`.
    OrderStatus, "order status", {
        Open => "open",
        TriggerPending => "trigger pending",
        Complete => "complete",
        Rejected => "rejected",
        Cancelled => "cancelled",
    }
);

impl OrderStatus {
    /// Orders a cancel-all should touch.
    pub fn is_pending(self) -> bool {
        matches!(self, OrderStatus::Open | OrderStatus::TriggerPending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_constant_round_trips_through_its_web_string() {
        for e in Exchange::ALL {
            assert_eq!(e.as_str().parse::<Exchange>().unwrap(), *e);
            assert_eq!(e.to_string(), e.as_str());
        }
        for p in Product::ALL {
            assert_eq!(p.as_str().parse::<Product>().unwrap(), *p);
        }
        for p in PriceType::ALL {
            assert_eq!(p.as_str().parse::<PriceType>().unwrap(), *p);
        }
        for a in Action::ALL {
            assert_eq!(a.as_str().parse::<Action>().unwrap(), *a);
        }
        for v in Validity::ALL {
            assert_eq!(v.as_str().parse::<Validity>().unwrap(), *v);
        }
        for s in OrderStatus::ALL {
            assert_eq!(s.as_str().parse::<OrderStatus>().unwrap(), *s);
        }
    }

    #[test]
    fn exact_web_strings() {
        let ex: Vec<&str> = Exchange::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            ex,
            [
                "NSE",
                "NFO",
                "CDS",
                "BSE",
                "BFO",
                "BCD",
                "MCX",
                "NCDEX",
                "NCO",
                "NSE_INDEX",
                "BSE_INDEX",
                "MCX_INDEX",
                "GLOBAL_INDEX",
                "CRYPTO"
            ]
        );
        assert_eq!(PriceType::SlM.as_str(), "SL-M");
        assert_eq!(OrderStatus::TriggerPending.as_str(), "trigger pending");
    }

    #[test]
    fn parsing_is_case_sensitive_like_the_web() {
        assert!("nse".parse::<Exchange>().is_err());
        assert!("buy".parse::<Action>().is_err());
        let e = "XYZ".parse::<Exchange>().unwrap_err();
        assert_eq!(e.to_string(), "Invalid exchange 'XYZ'");
    }

    #[test]
    fn serde_uses_web_strings() {
        let v = serde_json::to_string(&PriceType::SlM).unwrap();
        assert_eq!(v, "\"SL-M\"");
        let p: Product = serde_json::from_str("\"NRML\"").unwrap();
        assert_eq!(p, Product::Nrml);
        assert!(serde_json::from_str::<Product>("\"BO\"").is_err());
    }

    #[test]
    fn exchange_classes() {
        assert!(Exchange::NseIndex.is_index());
        assert!(!Exchange::Nse.is_index());
        assert!(Exchange::Mcx.is_derivative());
        assert!(Exchange::Bse.is_cash());
        assert_eq!(Action::Buy.opposite(), Action::Sell);
        assert!(OrderStatus::TriggerPending.is_pending());
        assert!(!OrderStatus::Complete.is_pending());
    }
}
