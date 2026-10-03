//! Groww instrument master (web `database/master_contract_db.py`).
//!
//! `https://growwapi-assets.groww.in/instruments/instrument.csv`, parsed by
//! header name. Rules, all from the web:
//! * `exchange` -> `brexchange`; `exchange_token` -> `token` (string,
//!   leading zeros kept); `trading_symbol` -> `brsymbol` and initial
//!   `symbol`;
//! * exchange: `NSE`/`BSE` + segment `FNO` -> `NFO`/`BFO`; segment or
//!   instrument type `IDX` -> `NSE_INDEX`/`BSE_INDEX`; others keep the CSV
//!   exchange;
//! * instrument type: EQ, IDX->INDEX, FUT, CE, PE, ETF->EQ, CURR->CUR,
//!   COM->COM; missing: CASH->EQ, FNO strike>0 -> OPT, else FUT;
//! * expiry `yyyy-mm-dd` -> `DD-MMM-YY`; lot size NaN -> 1; strike NaN -> 0;
//!   tick size NaN -> 0.05;
//! * index renames on `symbol` only; NSE F&O symbols rebuilt as
//!   `[underlying][DDMMMYY]FUT` / `[underlying][DDMMMYY][strike]CE|PE`;
//!   BFO rows keep Groww's symbol (quirk 9.10); NFO options whose broker
//!   symbol has spaces get the spaces removed; rows with no symbol dropped.
//!
//! Deviation: the web leaves `name` empty (quirk 9.9). Here it is the
//! underlying for derivatives and Groww's `name` otherwise, so expiry
//! pickers and option chains (keyed on `name`) work.

use super::GrowwCore;
use crate::brokers::common::http;
use crate::brokers::common::master_contract::{
    expiry_compact, format_expiry, format_strike, rename, split_csv_line, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};

pub const MASTER_URL: &str = "https://growwapi-assets.groww.in/instruments/instrument.csv";

/// Groww index names -> OpenAlgo symbols (applied to `symbol` only).
pub const INDEX_RENAMES: &[(&str, &str)] = &[
    ("NIFTYJR", "NIFTYNXT50"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("NIFTYMIDCAP", "NIFTYMIDCAP100"),
    ("NIFTYSMALL", "NIFTYSMLCAP100"),
    ("NIFTYSMALLCAP250", "NIFTYSMLCAP250"),
    ("NIFTYCDTY", "NIFTYCOMMODITIES"),
    ("MIDCAP50", "NIFTYMIDCAP50"),
    ("BSESMLCAP", "BSESMALLCAP"),
];

const REQUIRED: &[&str] = &[
    "exchange",
    "exchange_token",
    "trading_symbol",
    "groww_symbol",
    "name",
    "instrument_type",
    "segment",
    "underlying_symbol",
    "expiry_date",
    "strike_price",
    "lot_size",
    "tick_size",
];

fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("nan") {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn instrument_type(raw: &str, segment: &str, strike: Option<f64>) -> &'static str {
    match raw {
        "EQ" | "ETF" => "EQ",
        "IDX" => "INDEX",
        "FUT" => "FUT",
        "CE" => "CE",
        "PE" => "PE",
        "CURR" => "CUR",
        "COM" => "COM",
        _ => match segment {
            "CASH" => "EQ",
            "FNO" if strike.unwrap_or(0.0) > 0.0 => "OPT",
            "FNO" => "FUT",
            _ => "EQ",
        },
    }
}

/// Parse the CSV into master rows.
pub fn parse_instruments(csv: &str) -> Result<Vec<SymToken>> {
    let mut lines = csv.lines();
    let header = CsvHeader::parse(lines.next().unwrap_or(""));
    let mut idx = Vec::with_capacity(REQUIRED.len());
    for name in REQUIRED {
        match header.index(name) {
            Some(i) => idx.push(i),
            None => {
                tracing::error!("Groww instrument file has no '{}' column", name);
                return Err(AppError::Broker(
                    "Groww's instrument list arrived in an unexpected format. Try downloading the master contract again later."
                        .into(),
                ));
            }
        }
    }
    fn field<'a>(f: &'a [String], idx: &[usize], n: usize) -> &'a str {
        f.get(idx[n]).map(|s| s.trim()).unwrap_or("")
    }
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let col = |n: usize| field(&f, &idx, n);
        let exchange_csv = col(0);
        let token = col(1);
        let trading_symbol = col(2);
        let name = col(4);
        let raw_type = col(5);
        let segment = col(6);
        let underlying = col(7);
        let expiry_raw = col(8);
        let strike = parse_num(col(9));
        let lot = parse_num(col(10)).map(|v| v as i32).unwrap_or(1);
        let tick = parse_num(col(11)).unwrap_or(0.05);

        let is_index = segment == "IDX" || raw_type == "IDX";
        let mut itype = instrument_type(raw_type, segment, strike);
        if is_index {
            itype = "INDEX";
        }
        let exchange = match (exchange_csv, segment, is_index) {
            ("NSE", _, true) => "NSE_INDEX",
            ("BSE", _, true) => "BSE_INDEX",
            ("NSE", "FNO", _) => "NFO",
            ("BSE", "FNO", _) => "BFO",
            (e, _, _) => e,
        };
        let expiry = chrono::NaiveDate::parse_from_str(
            expiry_raw.split([' ', 'T']).next().unwrap_or(""),
            "%Y-%m-%d",
        )
        .map(format_expiry)
        .unwrap_or_default();
        let strike = strike.unwrap_or(0.0);

        let mut symbol = rename(INDEX_RENAMES, trading_symbol)
            .unwrap_or(trading_symbol)
            .to_string();
        if exchange_csv == "NSE" && segment == "FNO" && !expiry.is_empty() {
            let base = if underlying.is_empty() {
                symbol.as_str()
            } else {
                underlying
            };
            let compact = expiry_compact(&expiry);
            match (itype, raw_type) {
                ("FUT", _) => symbol = format!("{}{}FUT", base, compact),
                (_, "CE") | (_, "PE") => {
                    symbol = format!("{}{}{}{}", base, compact, format_strike(strike), raw_type)
                }
                _ => {}
            }
        }
        if exchange == "NFO" && matches!(itype, "CE" | "PE") && trading_symbol.contains(' ') {
            symbol = trading_symbol.replace(' ', "");
        }
        let brsymbol = if trading_symbol.is_empty() {
            symbol.clone()
        } else {
            trading_symbol.to_string()
        };
        if symbol.trim().is_empty() {
            continue;
        }
        let name = if segment == "FNO" && !underlying.is_empty() {
            underlying.to_string()
        } else if !name.is_empty() {
            name.to_string()
        } else {
            symbol.clone()
        };
        out.push(SymToken {
            symbol,
            brsymbol,
            name,
            exchange: exchange.to_string(),
            brexchange: exchange_csv.to_string(),
            token: token.to_string(),
            expiry,
            strike,
            lot_size: lot,
            instrument_type: itype.to_string(),
            tick_size: tick,
        });
    }
    if out.is_empty() {
        return Err(AppError::Broker(
            "Groww's instrument list was empty. Try downloading the master contract again later."
                .into(),
        ));
    }
    Ok(out)
}

pub(crate) async fn download(core: &GrowwCore) -> Result<Vec<SymToken>> {
    download_from(core, MASTER_URL).await
}

pub(crate) async fn download_from(core: &GrowwCore, url: &str) -> Result<Vec<SymToken>> {
    let resp = core
        .http
        .get(url)
        .timeout(http::DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "Groww instrument download refused"
        );
        return Err(AppError::Broker(
            "Groww's instrument list could not be downloaded. Try again shortly.".into(),
        ));
    }
    let text = resp.text().await?;
    // The text is dropped when parsing returns; only the rows are kept.
    parse_instruments(&text)
}
