//! F Prime JSON dictionary model and F Prime type encoding and decoding.
//!
//! Values are `serde_json::Value`, encoded as the network API requires (API-4):
//! 64-bit integers are decimal strings, non-finite floats are the strings
//! `NaN`, `Infinity` and `-Infinity`, enumerations are their constant name.

mod codec;
mod input;
mod load;
mod pyjson;
mod text;
mod types;

pub use codec::{EncodedCommand, Reader, TypeError};
pub use input::{is_decimal_digit, python_decimal, python_float};
pub use load::DictError;
pub use types::{Channel, Command, Event, Member, Param, Time, Type};

use serde_json::Value;
use std::collections::HashMap;

/// A loaded dictionary: definitions by identifier and by name, and the type
/// configuration (sizes of the framework types) the wire format depends on.
#[derive(Debug)]
pub struct Dictionary {
    /// The dictionary file as loaded, served unchanged by the API.
    pub json: Value,
    pub commands: Vec<Command>,
    pub events: Vec<Event>,
    pub channels: Vec<Channel>,
    /// Named types, fully resolved.
    pub types: HashMap<String, Type>,
    pub(crate) command_by_name: HashMap<String, usize>,
    pub(crate) command_by_opcode: HashMap<u32, usize>,
    pub(crate) event_by_id: HashMap<u32, usize>,
    pub(crate) channel_by_id: HashMap<u32, usize>,
    /// Telemetry packet identifier to the indexes of its channels, in packet order.
    pub(crate) packets: HashMap<u32, Vec<usize>>,
    /// Bytes that encode boolean true and false.
    pub(crate) bool_true: u8,
    pub(crate) bool_false: u8,
    /// Width of a string length prefix (`FwSizeStoreType`).
    pub(crate) size_store: Type,
}

impl Dictionary {
    pub fn command(&self, name: &str) -> Option<&Command> {
        self.command_by_name.get(name).map(|i| &self.commands[*i])
    }

    pub fn command_by_opcode(&self, opcode: u32) -> Option<&Command> {
        self.command_by_opcode.get(&opcode).map(|i| &self.commands[*i])
    }

    pub fn event(&self, id: u32) -> Option<&Event> {
        self.event_by_id.get(&id).map(|i| &self.events[*i])
    }

    pub fn channel(&self, id: u32) -> Option<&Channel> {
        self.channel_by_id.get(&id).map(|i| &self.channels[*i])
    }

    /// The channels of a telemetry packet, in packet order.
    pub fn packet(&self, id: u32) -> Option<Vec<&Channel>> {
        let members = self.packets.get(&id)?;
        Some(members.iter().map(|i| &self.channels[*i]).collect())
    }

    /// A framework type such as `FwOpcodeType`, or `default` when the
    /// dictionary does not define it.
    pub fn config_type(&self, name: &str, default: Type) -> Type {
        self.types.get(name).cloned().unwrap_or(default)
    }

    /// The `projectVersion` of the dictionary metadata, empty when absent.
    pub fn version(&self) -> &str {
        self.json["metadata"]["projectVersion"].as_str().unwrap_or("")
    }
}
