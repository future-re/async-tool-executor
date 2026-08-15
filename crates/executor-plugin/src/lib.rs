//! Manifest, package management, and persistent-process adapters for ATE plugins.

mod manifest;
mod package;
mod protocol;
#[cfg(unix)]
mod supervisor;

pub use manifest::{PluginManifest, PluginToolManifest};
pub use package::{InstalledPlugin, PluginStore, StoreError, pack_directory, validate_package};
pub use protocol::{PLUGIN_PROTOCOL_VERSION, PluginRequest, PluginResponse};
#[cfg(unix)]
pub use supervisor::{ExternalToolAdapter, PluginRuntimeConfig, PluginSupervisor};
