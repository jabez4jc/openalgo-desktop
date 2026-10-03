//! Fyers master contract (web `database/master_contract_db.py`).
//!
//! * `NSE_CM.csv`, `BSE_CM.csv`, `NSE_FO.csv`, `BSE_FO.csv`: 21 columns, no
//!   header (layout below).
//! * `NSE_CD_sym_master.json`, `MCX_COM_sym_master.json`: the JSON masters,
//!   whose `qtyMultiplier` is the real lot (the CSVs say 1 for every CDS and
//!   MCX row).
//! * `index_hsm_mapping.json`: index display names, stored in `name` for
//!   index rows because the HSM feed subscribes indices by that name.
//!
//! Derivative symbols are `NAME + DD + MMM + YY (+ strike + CE/PE)` built
//! from "Symbol Details" (`BANKNIFTY 27 Oct 26 FUT` -> `BANKNIFTY27OCT26FUT`).

use super::FyersBroker;
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::http;
use crate::brokers::common::master_contract::{format_expiry, split_csv_line};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

/// CSV column positions (web `headers`).
pub mod col {
    pub const FYTOKEN: usize = 0;
    pub const SYMBOL_DETAILS: usize = 1;
    pub const INSTRUMENT_TYPE: usize = 2;
    pub const LOT_SIZE: usize = 3;
    pub const TICK_SIZE: usize = 4;
    pub const EXPIRY: usize = 8;
    pub const TICKER: usize = 9;
    pub const UNDERLYING: usize = 13;
    pub const STRIKE: usize = 15;
    pub const OPTION_TYPE: usize = 16;
}

/// web `bse_index_map` (Fyers underlying -> OpenAlgo `BSE_INDEX` symbol).
pub const BSE_INDEX_MAP: &[(&str, &str)] = &[
    ("SENSEX", "SENSEX"),
    ("SENSEX50", "SENSEX50"),
    ("BANKEX", "BANKEX"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("100", "BSE100"),
    ("200", "BSE200"),
    ("500", "BSE500"),
    ("150MIDCAP", "BSE150MIDCAPINDEX"),
    ("250LARGEMIDCAP", "BSE250LARGEMIDCAPINDEX"),
    ("400MIDSMALLCAP", "BSE400MIDSMALLCAPINDEX"),
    ("AUTO", "BSEAUTO"),
    ("CG", "BSECAPITALGOODS"),
    ("CARBONEX", "BSECARBONEX"),
    ("CD", "BSECONSUMERDURABLES"),
    ("CPSE", "BSECPSE"),
    ("DOL100", "BSEDOLLEX100"),
    ("DOL200", "BSEDOLLEX200"),
    ("DOL30", "BSEDOLLEX30"),
    ("ENERGY", "BSEENERGY"),
    ("FMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("GREENEX", "BSEGREENEX"),
    ("HC", "BSEHEALTHCARE"),
    ("INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("IT", "BSEINFORMATIONTECHNOLOGY"),
    ("IPO", "BSEIPO"),
    ("LRGCAP", "BSELARGECAP"),
    ("METAL", "BSEMETAL"),
    ("MIDCAP", "BSEMIDCAP"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("PSU", "BSEPSU"),
    ("REALTY", "BSEREALTY"),
    ("SMLCAP", "BSESMALLCAP"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("SME IPO", "BSESMEIPO"),
    ("TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
    ("100LARGECAPTMC", "BSE100LARGECAPTMCINDEX"),
    ("250SMALLCAP", "BSE250SMALLCAPINDEX"),
    ("ALLCAP", "BSEALLCAP"),
    ("BASMTR", "BSEBASICMATERIALS"),
    ("BHRT22", "BSEBHARAT22INDEX"),
    ("BSHOSP", "BSEHOSPITALS"),
    ("CDGS", "BSECONSUMERDISCRETIONARYGOODS&SERVICES"),
    ("DFRG", "BSEDIVERSIFIEDFINANCIALSREVENUEGROWTHINDEX"),
    ("DIVIDENDSTABILITY", "BSEDIVIDENDSTABILITY"),
    ("ENHANCEDVALUE", "BSEENHANCEDVALUE"),
    ("ESG100", "BSE100ESG"),
    ("FOCIT", "BSEFOCUSEDIT"),
    ("INDIAMANUFACTURING", "BSEINDIAMANUFACTURING"),
    ("LOWVOLATILITY", "BSELOWVOLATILITY"),
    ("MOMENTUM", "BSEMOMENTUM"),
    ("PRIVATEBANKS", "BSEPRIVATEBANKS"),
    ("QUALITY", "BSEQUALITY"),
    ("UTILS", "BSEUTILITIES"),
];

/// `NSE_INDEX` symbol from the Fyers underlying: no spaces or hyphens, then
/// the web's override (`NIFTYMID50` -> `NIFTYMIDCAP50`).
pub fn nse_index_symbol(underlying: &str) -> String {
    let s = underlying.replace([' ', '-'], "");
    if s == "NIFTYMID50" {
        "NIFTYMIDCAP50".into()
    } else {
        s
    }
}

/// `BSE_INDEX` symbol: the map, else upper case without spaces.
pub fn bse_index_symbol(underlying: &str) -> String {
    BSE_INDEX_MAP
        .iter()
        .find(|(k, _)| *k == underlying)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| underlying.to_uppercase().replace(' ', ""))
}

/// web `reformat_symbol_detail`: `"BANKNIFTY 27 Oct 26 71900 CE"` ->
/// `BANKNIFTY27OCT2671900` (the first five words; the option type is
/// appended by the caller). `None` when there are fewer than five words.
pub fn reformat_symbol_detail(details: &str) -> Option<String> {
    let p: Vec<&str> = details.split_whitespace().collect();
    if p.len() < 5 {
        return None;
    }
    Some(format!(
        "{}{}{}{}{}",
        p[0],
        p[1],
        p[2].to_uppercase(),
        p[3],
        p[4]
    ))
}

/// Epoch seconds -> `DD-MMM-YY` (UTC date, like pandas `unit="s"`).
pub fn expiry_from_epoch(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| format_expiry(d.date_naive()))
        .unwrap_or_default()
}

/// The index name the HSM feed subscribes with: the published display name,
/// else the ticker stem (`NSE:NIFTYIT-INDEX` -> `NIFTYIT`).
pub fn index_feed_name(brsymbol: &str, index_names: &HashMap<String, String>) -> String {
    index_names.get(brsymbol).cloned().unwrap_or_else(|| {
        brsymbol
            .split_once(':')
            .map(|(_, s)| s)
            .unwrap_or(brsymbol)
            .replace("-INDEX", "")
    })
}

fn parse_num<T: std::str::FromStr>(s: &str) -> Option<T> {
    s.trim().parse().ok()
}

fn lot(s: &str) -> i32 {
    parse_num::<f64>(s).map(|v| v as i32).unwrap_or(1)
}

/// `NSE_CM.csv` / `BSE_CM.csv` (`cash` is `NSE` or `BSE`): equities (NSE
/// types 0, 9 and `-GB` bonds of type 2; BSE types 0, 4, 50) and indices
/// (type 10, on `NSE_INDEX` / `BSE_INDEX`), all typed `EQ`.
pub fn parse_cash_csv(
    text: &str,
    cash: &str,
    index_names: &HashMap<String, String>,
) -> Vec<SymToken> {
    let (eq_types, index_ex): (&[i64], &str) = match cash {
        "NSE" => (&[0, 9], "NSE_INDEX"),
        _ => (&[0, 4, 50], "BSE_INDEX"),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        if f.len() <= col::OPTION_TYPE {
            continue;
        }
        let get = |i: usize| f[i].trim();
        let Some(itype) = parse_num::<i64>(get(col::INSTRUMENT_TYPE)) else {
            continue;
        };
        let ticker = get(col::TICKER);
        let exchange = if eq_types.contains(&itype)
            || (cash == "NSE" && itype == 2 && ticker.ends_with("-GB"))
        {
            cash
        } else if itype == 10 {
            index_ex
        } else {
            continue;
        };
        let underlying = get(col::UNDERLYING);
        let (symbol, name) = match exchange {
            "NSE_INDEX" => (
                nse_index_symbol(underlying),
                index_feed_name(ticker, index_names),
            ),
            "BSE_INDEX" => (
                bse_index_symbol(underlying),
                index_feed_name(ticker, index_names),
            ),
            _ => (underlying.to_string(), get(col::SYMBOL_DETAILS).to_string()),
        };
        if symbol.is_empty() || get(col::FYTOKEN).is_empty() {
            continue;
        }
        out.push(SymToken {
            symbol,
            brsymbol: ticker.to_string(),
            name,
            exchange: exchange.to_string(),
            brexchange: cash.to_string(),
            token: get(col::FYTOKEN).to_string(),
            expiry: String::new(),
            strike: parse_num(get(col::STRIKE)).unwrap_or(0.0),
            lot_size: lot(get(col::LOT_SIZE)),
            instrument_type: "EQ".into(),
            tick_size: parse_num(get(col::TICK_SIZE)).unwrap_or(0.0),
        });
    }
    out
}

/// One derivative row (CSV or JSON master).
struct Derivative<'a> {
    token: &'a str,
    details: &'a str,
    name: &'a str,
    ticker: &'a str,
    option_type: &'a str,
    expiry_epoch: i64,
    strike: f64,
    lot: i32,
    tick: f64,
    exchange: &'a str,
}

fn derivative(d: Derivative<'_>) -> Option<SymToken> {
    // web: XX (and, on BFO, a blank option type) is a future.
    let instrument_type = match d.option_type {
        "CE" | "PE" => d.option_type,
        "XX" | "" => "FUT",
        other => {
            tracing::debug!("Skipping Fyers row with option type {}", other);
            return None;
        }
    };
    let base = reformat_symbol_detail(d.details)?;
    let symbol = match instrument_type {
        "FUT" => base,
        ot => format!("{}{}", base, ot),
    };
    if d.token.is_empty() || d.ticker.is_empty() {
        return None;
    }
    Some(SymToken {
        symbol,
        brsymbol: d.ticker.to_string(),
        name: d.name.to_string(),
        exchange: d.exchange.to_string(),
        brexchange: d.exchange.to_string(),
        token: d.token.to_string(),
        expiry: expiry_from_epoch(d.expiry_epoch),
        strike: d.strike,
        lot_size: d.lot,
        instrument_type: instrument_type.to_string(),
        tick_size: d.tick,
    })
}

/// `NSE_FO.csv` (`NFO`) / `BSE_FO.csv` (`BFO`).
pub fn parse_fo_csv(text: &str, exchange: &str) -> Vec<SymToken> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        if f.len() <= col::OPTION_TYPE {
            continue;
        }
        let get = |i: usize| f[i].trim();
        let option_type = get(col::OPTION_TYPE);
        // NSE_FO always types its rows; only BFO leaves futures blank.
        if exchange == "NFO" && option_type.is_empty() {
            continue;
        }
        if let Some(r) = derivative(Derivative {
            token: get(col::FYTOKEN),
            details: get(col::SYMBOL_DETAILS),
            name: get(col::SYMBOL_DETAILS),
            ticker: get(col::TICKER),
            option_type,
            expiry_epoch: parse_num::<f64>(get(col::EXPIRY))
                .map(|v| v as i64)
                .unwrap_or(0),
            strike: parse_num(get(col::STRIKE)).unwrap_or(0.0),
            lot: lot(get(col::LOT_SIZE)),
            tick: parse_num(get(col::TICK_SIZE)).unwrap_or(0.0),
            exchange,
        }) {
            out.push(r);
        }
    }
    out
}

/// One row of `NSE_CD_sym_master.json` / `MCX_COM_sym_master.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JsonRow {
    #[serde(rename = "fyToken", deserialize_with = "string_lenient")]
    pub fy_token: String,
    /// Description column the web stores as `name`.
    #[serde(rename = "symbolDetails", deserialize_with = "string_lenient")]
    pub symbol_details: String,
    #[serde(rename = "symDetails", deserialize_with = "string_lenient")]
    pub sym_details: String,
    #[serde(rename = "symTicker", deserialize_with = "string_lenient")]
    pub sym_ticker: String,
    #[serde(rename = "optType", deserialize_with = "string_lenient")]
    pub opt_type: String,
    #[serde(rename = "expiryDate", deserialize_with = "i64_lenient")]
    pub expiry_date: i64,
    #[serde(rename = "strikePrice", deserialize_with = "f64_lenient")]
    pub strike_price: f64,
    #[serde(rename = "qtyMultiplier", deserialize_with = "f64_lenient")]
    pub qty_multiplier: f64,
    #[serde(rename = "tickSize", deserialize_with = "f64_lenient")]
    pub tick_size: f64,
}

/// A JSON master (`{ticker: row, ...}`) for `CDS` or `MCX`; lots from
/// `qtyMultiplier`.
pub fn parse_json_master(text: &str, exchange: &str) -> Result<Vec<SymToken>> {
    let map: HashMap<String, Value> = serde_json::from_str(text).map_err(|e| {
        tracing::warn!("Fyers {} master is not readable JSON: {}", exchange, e);
        AppError::Broker(
            "The Fyers master contract download was incomplete. Try downloading it again.".into(),
        )
    })?;
    let mut rows: Vec<JsonRow> = map
        .into_values()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    // HashMap order is random; keep the output stable.
    rows.sort_by(|a, b| a.fy_token.cmp(&b.fy_token));
    Ok(rows
        .iter()
        .filter_map(|r| {
            // Only CE/PE/XX are typed rows on the JSON masters.
            if r.opt_type.is_empty() {
                return None;
            }
            let name = if r.symbol_details.is_empty() {
                &r.sym_details
            } else {
                &r.symbol_details
            };
            derivative(Derivative {
                token: &r.fy_token,
                details: &r.sym_details,
                name,
                ticker: &r.sym_ticker,
                option_type: &r.opt_type,
                expiry_epoch: r.expiry_date,
                strike: r.strike_price,
                lot: r.qty_multiplier as i32,
                tick: r.tick_size,
                exchange,
            })
        })
        .collect())
}

/// Files in web download order.
pub const FILES: &[(&str, &str)] = &[
    ("NSE_CD", "NSE_CD_sym_master.json"),
    ("NSE_FO", "NSE_FO.csv"),
    ("NSE_CM", "NSE_CM.csv"),
    ("BSE_CM", "BSE_CM.csv"),
    ("BSE_FO", "BSE_FO.csv"),
    ("MCX_COM", "MCX_COM_sym_master.json"),
];

/// Parse the downloaded files (keyed as in `FILES`) in the web's insert
/// order: NSE cash, BSE cash, BFO, NFO, CDS, MCX.
pub fn parse_all(
    files: &HashMap<&str, String>,
    index_names: &HashMap<String, String>,
) -> Result<Vec<SymToken>> {
    let get = |k: &str| files.get(k).map(String::as_str).unwrap_or("");
    let mut all = Vec::new();
    all.extend(parse_cash_csv(get("NSE_CM"), "NSE", index_names));
    all.extend(parse_cash_csv(get("BSE_CM"), "BSE", index_names));
    all.extend(parse_fo_csv(get("BSE_FO"), "BFO"));
    all.extend(parse_fo_csv(get("NSE_FO"), "NFO"));
    all.extend(parse_json_master(get("NSE_CD"), "CDS")?);
    all.extend(parse_json_master(get("MCX_COM"), "MCX")?);
    Ok(all)
}

async fn fetch(b: &FyersBroker, file: &str) -> Result<String> {
    let url = format!("{}/{}", b.urls().public, file);
    let resp = b
        .http
        .get(&url)
        .timeout(http::DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "Fyers master file {} failed",
            file
        );
        return Err(AppError::Broker(
            "Fyers did not send the master contract. The existing symbols are kept; try again shortly."
                .into(),
        ));
    }
    Ok(resp.text().await?)
}

/// web `fetch_index_hsm_names`: `{}` when unavailable.
async fn index_names(b: &FyersBroker) -> HashMap<String, String> {
    match fetch(b, "index_hsm_mapping.json").await {
        Ok(text) => match serde_json::from_str::<HashMap<String, Value>>(&text) {
            Ok(m) => m
                .into_iter()
                .map(|(k, v)| {
                    let s = match v {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (k, s)
                })
                .collect(),
            Err(e) => {
                tracing::warn!(
                    "Fyers index name table unreadable, using ticker stems: {}",
                    e
                );
                HashMap::new()
            }
        },
        Err(_) => {
            tracing::warn!("Fyers index name table unavailable, using ticker stems");
            HashMap::new()
        }
    }
}

/// Download every file first; any failure keeps the existing master (web).
pub async fn download(b: &FyersBroker) -> Result<Vec<SymToken>> {
    let mut files: HashMap<&str, String> = HashMap::new();
    for (key, file) in FILES {
        files.insert(key, fetch(b, file).await?);
    }
    let names = index_names(b).await;
    let all = parse_all(&files, &names)?;
    drop(files);
    tracing::info!("Fyers master contract parsed: {} instruments", all.len());
    Ok(all)
}
