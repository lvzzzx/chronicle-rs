use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use arrow::array::{Array, Int16Array, Int64Array, Int8Array, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct SymbolIdentity {
    pub symbol_id: u64,
    pub symbol_code: String,
    pub exchange_symbol: String,
    pub exchange_id: u16,
    pub asset_type: u8,
    pub symbol_version_id: u64,
    pub valid_from_us: i64,
    pub valid_until_us: i64,
}

#[derive(Debug, Clone)]
struct SymbolVersion {
    symbol_id: u64,
    symbol_code: String,
    exchange_symbol: String,
    exchange_id: u16,
    asset_type: u8,
    valid_from_us: i64,
    valid_until_us: i64,
    symbol_version_id: u64,
}

#[derive(Debug, Clone)]
pub struct SymbolCatalog {
    by_market: HashMap<(u16, u32), Vec<SymbolVersion>>,
}

impl SymbolCatalog {
    pub fn load_delta(path: impl AsRef<Path>) -> Result<Self> {
        let table_path = path.as_ref();
        let files = list_active_delta_files(table_path)?;
        let mut by_market: HashMap<(u16, u32), Vec<SymbolVersion>> = HashMap::new();

        for file in files {
            load_parquet_file(&file, &mut by_market)?;
        }

        for versions in by_market.values_mut() {
            versions.sort_by_key(|v| v.valid_from_us);
        }

        Ok(Self { by_market })
    }

    pub fn resolve_by_market_id(
        &self,
        exchange_id: u16,
        market_id: u32,
        event_time_ns: u64,
    ) -> Option<SymbolIdentity> {
        let event_time_us = i64::try_from(event_time_ns / 1_000).ok()?;
        let versions = self.by_market.get(&(exchange_id, market_id))?;
        // Find the latest version with valid_from <= event_time < valid_until
        let mut best: Option<&SymbolVersion> = None;
        for version in versions {
            if event_time_us < version.valid_from_us {
                break;
            }
            if event_time_us >= version.valid_from_us && event_time_us < version.valid_until_us {
                best = Some(version);
            }
        }
        best.map(|v| SymbolIdentity {
            symbol_id: v.symbol_id,
            symbol_code: v.symbol_code.clone(),
            exchange_symbol: v.exchange_symbol.clone(),
            exchange_id: v.exchange_id,
            asset_type: v.asset_type,
            symbol_version_id: v.symbol_version_id,
            valid_from_us: v.valid_from_us,
            valid_until_us: v.valid_until_us,
        })
    }

    pub fn market_id_for_symbol(symbol: &str) -> u32 {
        let h = fxhash64(symbol);
        ((h >> 32) as u32) ^ (h as u32)
    }
}

fn list_active_delta_files(table_path: &Path) -> Result<Vec<PathBuf>> {
    let log_dir = table_path.join("_delta_log");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&log_dir)
        .with_context(|| format!("read delta log directory {}", log_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().map(|ext| ext == "json").unwrap_or(false))
        .collect();
    entries.sort();

    let mut active: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        let file = File::open(&entry).with_context(|| format!("open {}", entry.display()))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line)
                .with_context(|| format!("parse delta log {}", entry.display()))?;
            if let Some(add) = value.get("add") {
                if let Some(path) = add.get("path").and_then(|v| v.as_str()) {
                    active.insert(path.to_string());
                }
            }
            if let Some(remove) = value.get("remove") {
                if let Some(path) = remove.get("path").and_then(|v| v.as_str()) {
                    active.remove(path);
                }
            }
        }
    }

    Ok(active
        .into_iter()
        .map(|rel| table_path.join(rel))
        .collect())
}

fn load_parquet_file(
    file_path: &Path,
    by_market: &mut HashMap<(u16, u32), Vec<SymbolVersion>>,
) -> Result<()> {
    let file = File::open(file_path)
        .with_context(|| format!("open parquet {}", file_path.display()))?;
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)?
        .with_batch_size(2048)
        .build()?;

    while let Some(batch) = reader.next() {
        let batch = batch?;
        let exchange_id = get_array::<Int16Array>(&batch, "exchange_id")?;
        let exchange_symbol = get_array::<StringArray>(&batch, "exchange_symbol")?;
        let asset_type = get_array::<Int8Array>(&batch, "asset_type")?;
        let valid_from = get_array::<Int64Array>(&batch, "valid_from_ts")?;
        let valid_until = get_array::<Int64Array>(&batch, "valid_until_ts")?;
        let symbol_id = get_array::<Int64Array>(&batch, "symbol_id")?;

        for i in 0..batch.num_rows() {
            if exchange_id.is_null(i)
                || exchange_symbol.is_null(i)
                || asset_type.is_null(i)
                || valid_from.is_null(i)
                || valid_until.is_null(i)
                || symbol_id.is_null(i)
            {
                continue;
            }

            let exchange_id_val = exchange_id.value(i) as i32;
            if exchange_id_val < 0 {
                continue;
            }
            let exchange_id_val = exchange_id_val as u16;

            let asset_type_val = asset_type.value(i) as i16;
            if asset_type_val < 0 {
                continue;
            }
            let asset_type_val = asset_type_val as u8;

            let symbol_id_val = symbol_id.value(i);
            if symbol_id_val < 0 {
                continue;
            }
            let symbol_id_val = symbol_id_val as u64;

            let exchange_symbol_val = exchange_symbol.value(i).to_string();
            let market_id = SymbolCatalog::market_id_for_symbol(&exchange_symbol_val);
            let symbol_code = normalize_symbol_code(&exchange_symbol_val);

            let valid_from_us = valid_from.value(i);
            let valid_until_us = valid_until.value(i);
            if valid_until_us <= valid_from_us {
                continue;
            }

            let symbol_version_id =
                fxhash64(&format!("{symbol_id_val}:{valid_from_us}:{valid_until_us}"));

            let version = SymbolVersion {
                symbol_id: symbol_id_val,
                symbol_code,
                exchange_symbol: exchange_symbol_val,
                exchange_id: exchange_id_val,
                asset_type: asset_type_val,
                valid_from_us,
                valid_until_us,
                symbol_version_id,
            };
            by_market
                .entry((exchange_id_val, market_id))
                .or_default()
                .push(version);
        }
    }

    Ok(())
}

fn get_array<'a, T: Array + 'static>(
    batch: &'a arrow::record_batch::RecordBatch,
    name: &str,
) -> Result<&'a T> {
    let column = batch
        .schema()
        .column_with_name(name)
        .ok_or_else(|| anyhow!("missing column {name}"))?
        .0;
    batch
        .column(column)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| anyhow!("column {name} has unexpected type"))
}

fn normalize_symbol_code(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.chars() {
        let lowered = ch.to_ascii_lowercase();
        let mapped = match lowered {
            'a'..='z' | '0'..='9' | '.' | '_' => Some(lowered),
            '-' => Some('-'),
            '/' | ':' | ' ' => Some('-'),
            _ => None,
        };
        if let Some(c) = mapped {
            if c == '-' {
                if prev_dash || out.is_empty() {
                    prev_dash = true;
                    continue;
                }
                prev_dash = true;
                out.push('-');
            } else {
                prev_dash = false;
                out.push(c);
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn fxhash64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1099511628211904);
    }
    hash
}
