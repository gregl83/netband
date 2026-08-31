use std::collections::{BTreeMap, HashSet};
use std::io;
use std::net::IpAddr;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInterface {
    pub name: String,
    pub addresses: Vec<IpAddr>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum InterfaceError {
    #[error("cannot inspect network interfaces: {message}")]
    Inspect { message: String },
    #[error("network interface does not exist: {name}")]
    Missing { name: String },
    #[error("network interface is not up: {name}")]
    Down { name: String },
    #[error("network interface has no usable source address: {name}")]
    Addressless { name: String },
}

pub trait InterfaceResolver: Send + Sync {
    fn resolve(&self, name: &str) -> Result<ResolvedInterface, InterfaceError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemInterfaceResolver;

impl InterfaceResolver for SystemInterfaceResolver {
    fn resolve(&self, name: &str) -> Result<ResolvedInterface, InterfaceError> {
        resolve_from(name, if_addrs::get_if_addrs().map_err(inspect_error)?)
    }
}

pub fn resolve_configured(names: &[String]) -> Result<Vec<ResolvedInterface>, InterfaceError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let interfaces = if_addrs::get_if_addrs().map_err(inspect_error)?;
    names
        .iter()
        .map(|name| resolve_from(name, interfaces.clone()))
        .collect()
}

fn resolve_from(
    name: &str,
    interfaces: Vec<if_addrs::Interface>,
) -> Result<ResolvedInterface, InterfaceError> {
    let named = interfaces
        .into_iter()
        .filter(|interface| interface.name == name)
        .collect::<Vec<_>>();
    if named.is_empty() {
        return Err(InterfaceError::Missing {
            name: name.to_owned(),
        });
    }
    if !named.iter().any(if_addrs::Interface::is_oper_up) {
        return Err(InterfaceError::Down {
            name: name.to_owned(),
        });
    }
    let mut addresses = named
        .into_iter()
        .filter(if_addrs::Interface::is_oper_up)
        .map(|interface| interface.ip())
        .filter(|address| !address.is_unspecified())
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(InterfaceError::Addressless {
            name: name.to_owned(),
        });
    }
    Ok(ResolvedInterface {
        name: name.to_owned(),
        addresses,
    })
}

fn inspect_error(error: io::Error) -> InterfaceError {
    InterfaceError::Inspect {
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FairnessState {
    attempts: u64,
    last_turn: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct FairInterfaceSelector {
    order: Vec<String>,
    state: BTreeMap<String, FairnessState>,
    turn: u64,
}

impl FairInterfaceSelector {
    pub fn new(interfaces: &[String]) -> Self {
        Self {
            order: interfaces.to_vec(),
            state: interfaces
                .iter()
                .cloned()
                .map(|interface| (interface, FairnessState::default()))
                .collect(),
            turn: 0,
        }
    }

    pub fn select<'a>(&'a self, eligible: &HashSet<String>) -> Option<&'a str> {
        self.order
            .iter()
            .enumerate()
            .filter(|(_, interface)| eligible.contains(interface.as_str()))
            .min_by_key(|(index, interface)| {
                let state = self
                    .state
                    .get(interface.as_str())
                    .copied()
                    .unwrap_or_default();
                (state.attempts, state.last_turn.unwrap_or(0), *index)
            })
            .map(|(_, interface)| interface.as_str())
    }

    pub fn record_attempt(&mut self, interface: &str) {
        if let Some(state) = self.state.get_mut(interface) {
            self.turn = self.turn.wrapping_add(1);
            state.attempts = state.attempts.saturating_add(1);
            state.last_turn = Some(self.turn);
        }
    }

    pub fn attempts(&self, interface: &str) -> u64 {
        self.state.get(interface).map_or(0, |state| state.attempts)
    }
}
