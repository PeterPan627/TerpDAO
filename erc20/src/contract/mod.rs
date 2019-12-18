mod prefixedstorage;

use schemars::JsonSchema;

use prefixedstorage::PrefixedStorage;
use std::convert::TryInto;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snafu::ResultExt;

use cosmwasm::errors::{ContractErr, DynContractErr, ParseErr, Result};
use cosmwasm::serde::from_slice;
use cosmwasm::storage::Storage;
use cosmwasm::types::{Params, QueryResponse, Response};

#[derive(Serialize, Deserialize, Clone, PartialEq, JsonSchema)]
pub struct InitialBalance {
    pub address: String,
    pub amount: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InitMsg {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub initial_balances: Vec<InitialBalance>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HandleMsg {
    Approve {
        spender: String,
        amount: String,
    },
    Transfer {
        recipient: String,
        amount: String,
    },
    TransferFrom {
        owner: String,
        recipient: String,
        amount: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase")]
// Unused for now. TODO: Create smart queries for balances and allowances that
// take human readable addresses once those issues are resolved
// 1. https://github.com/confio/cosmwasm/issues/73
// 2. https://github.com/confio/cosmwasm/issues/72
pub enum QueryMsg {}

pub const PREFIX_CONFIG: &[u8] = b"config";
pub const PREFIX_BALANCES: &[u8] = b"balances";
pub const PREFIX_ALLOWANCES: &[u8] = b"allowances";

pub const KEY_TOTAL_SUPPLY: &[u8] = b"total_supply";
pub const KEY_NAME: &[u8] = b"name";
pub const KEY_SYMBOL: &[u8] = b"symbol";
pub const KEY_DECIMALS: &[u8] = b"decimals";

pub fn init<T: Storage>(store: &mut T, _params: Params, msg: Vec<u8>) -> Result<Response> {
    let msg: InitMsg = from_slice(&msg).context(ParseErr { kind: "InitMsg" })?;

    let mut total_supply: u128 = 0;
    {
        // Initial balances
        let mut balances_store = PrefixedStorage::new(store, PREFIX_BALANCES);
        for row in msg.initial_balances {
            let raw_address = address_to_key(&row.address);
            let amount_raw = parse_u128(&row.amount)?;
            balances_store.set(&raw_address, &amount_raw.to_be_bytes());
            total_supply += amount_raw;
        }
    }

    let mut config_store = PrefixedStorage::new(store, PREFIX_CONFIG);

    // Name
    if !is_valid_name(&msg.name) {
        return ContractErr {
            msg: "Name is not in the expected format (3-30 UTF-8 bytes)",
        }
        .fail();
    }
    config_store.set(KEY_NAME, msg.name.as_bytes());

    // Symbol
    if !is_valid_symbol(&msg.symbol) {
        return ContractErr {
            msg: "Ticker symbol is not in expected format [A-Z]{3,6}",
        }
        .fail();
    }
    config_store.set(KEY_SYMBOL, msg.symbol.as_bytes());

    // Decimals
    if msg.decimals > 18 {
        return ContractErr {
            msg: "Decimals must not exceed 18",
        }
        .fail();
    }
    config_store.set(KEY_DECIMALS, &msg.decimals.to_be_bytes());

    // Total supply
    config_store.set(KEY_TOTAL_SUPPLY, &total_supply.to_be_bytes());

    Ok(Response::default())
}

pub fn handle<T: Storage>(store: &mut T, params: Params, msg: Vec<u8>) -> Result<Response> {
    let msg: HandleMsg = from_slice(&msg).context(ParseErr { kind: "HandleMsg" })?;

    match msg {
        HandleMsg::Approve { spender, amount } => try_approve(store, params, &spender, &amount),
        HandleMsg::Transfer { recipient, amount } => {
            try_transfer(store, params, &recipient, &amount)
        }
        HandleMsg::TransferFrom {
            owner,
            recipient,
            amount,
        } => try_transfer_from(store, params, &owner, &recipient, &amount),
    }
}

pub fn query<T: Storage>(_store: &T, msg: Vec<u8>) -> Result<QueryResponse> {
    let msg: QueryMsg = from_slice(&msg).context(ParseErr { kind: "QueryMsg" })?;
    match msg {}
}

fn try_transfer<T: Storage>(
    store: &mut T,
    params: Params,
    recipient: &str,
    amount: &str,
) -> Result<Response> {
    let sender_address_raw = address_to_key(&params.message.signer);
    let recipient_address_raw = address_to_key(&recipient);
    let amount_raw = parse_u128(amount)?;

    perform_transfer(
        store,
        &sender_address_raw,
        &recipient_address_raw,
        amount_raw,
    )?;

    let res = Response {
        messages: vec![],
        log: Some("transfer successful".to_string()),
        data: None,
    };
    Ok(res)
}

fn try_transfer_from<T: Storage>(
    store: &mut T,
    params: Params,
    owner: &str,
    recipient: &str,
    amount: &str,
) -> Result<Response> {
    let spender_address_raw = address_to_key(&params.message.signer);
    let owner_address_raw = address_to_key(&owner);
    let recipient_address_raw = address_to_key(&recipient);
    let amount_raw = parse_u128(amount)?;

    let mut allowance = read_allowance(store, &owner_address_raw, &spender_address_raw)?;
    if allowance < amount_raw {
        return DynContractErr {
            msg: format!(
                "Insufficient allowance: allowance={}, required={}",
                allowance, amount_raw
            ),
        }
        .fail();
    }
    allowance -= amount_raw;
    write_allowance(store, &owner_address_raw, &spender_address_raw, allowance);
    perform_transfer(
        store,
        &owner_address_raw,
        &recipient_address_raw,
        amount_raw,
    )?;

    let res = Response {
        messages: vec![],
        log: Some("transfer from successful".to_string()),
        data: None,
    };
    Ok(res)
}

fn try_approve<T: Storage>(
    store: &mut T,
    params: Params,
    spender: &str,
    amount: &str,
) -> Result<Response> {
    let owner_address_raw = address_to_key(&params.message.signer);
    let spender_address_raw = address_to_key(&spender);
    let amount_raw = parse_u128(amount)?;
    write_allowance(store, &owner_address_raw, &spender_address_raw, amount_raw);
    let res = Response {
        messages: vec![],
        log: Some("approve successful".to_string()),
        data: None,
    };
    Ok(res)
}

fn perform_transfer<T: Storage>(
    store: &mut T,
    from: &[u8; 32],
    to: &[u8; 32],
    amount: u128,
) -> Result<()> {
    let mut balances_store = PrefixedStorage::new(store, PREFIX_BALANCES);
    let mut from_balance = read_u128(&balances_store, from)?;

    if from_balance < amount {
        return DynContractErr {
            msg: format!(
                "Insufficient funds: balance={}, required={}",
                from_balance, amount
            ),
        }
        .fail();
    }

    let mut to_balance = read_u128(&balances_store, to)?;

    from_balance -= amount;
    to_balance += amount;

    balances_store.set(from, &from_balance.to_be_bytes());
    balances_store.set(to, &to_balance.to_be_bytes());

    Ok(())
}

// Converts 16 bytes value into u128
// Errors if data found that is not 16 bytes
pub fn bytes_to_u128(data: &[u8]) -> Result<u128> {
    match data[0..16].try_into() {
        Ok(bytes) => Ok(u128::from_be_bytes(bytes)),
        Err(_) => ContractErr {
            msg: "Corrupted data found. 16 byte expected.",
        }
        .fail(),
    }
}

// Reads 16 byte storage value into u128
// Returns zero if key does not exist. Errors if data found that is not 16 bytes
pub fn read_u128<T: Storage>(store: &T, key: &[u8]) -> Result<u128> {
    return match store.get(key) {
        Some(data) => bytes_to_u128(&data),
        None => Ok(0u128),
    };
}

pub fn parse_u128(decimal: &str) -> Result<u128> {
    match decimal.parse::<u128>() {
        Ok(value) => Ok(value),
        Err(_) => ContractErr {
            msg: "Error while parsing decimal string to u128",
        }
        .fail(),
    }
}

fn read_allowance<T: Storage>(store: &mut T, owner: &[u8; 32], spender: &[u8; 32]) -> Result<u128> {
    let allowances_store = PrefixedStorage::new(store, PREFIX_ALLOWANCES);
    let key = [&owner[..], &spender[..]].concat();
    return read_u128(&allowances_store, &key);
}

fn write_allowance<T: Storage>(
    store: &mut T,
    owner: &[u8; 32],
    spender: &[u8; 32],
    amount: u128,
) -> () {
    let mut allowances_store = PrefixedStorage::new(store, PREFIX_ALLOWANCES);
    let key = [&owner[..], &spender[..]].concat();
    allowances_store.set(&key, &amount.to_be_bytes());
}

// We assume the printable addresses we receive always have the same string representation
// TODO: Consider using faster, non-cryptographic hasher if relevant
pub fn address_to_key(printable: &str) -> [u8; 32] {
    let data = Sha256::digest(printable.as_bytes());
    let fixed_size_data: [u8; 32] = data[..].try_into().unwrap();
    return fixed_size_data;
}

fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 3 || bytes.len() > 30 {
        return false;
    }
    return true;
}

fn is_valid_symbol(symbol: &str) -> bool {
    let bytes = symbol.as_bytes();
    if bytes.len() < 3 || bytes.len() > 6 {
        return false;
    }

    for byte in bytes.iter() {
        if *byte < 65 || *byte > 90 {
            return false;
        }
    }

    return true;
}

#[cfg(test)]
mod tests;
