//! Sentinel core: everything that talks to ADB and the phone.
//!
//! This crate has no UI dependencies. A frontend (GPUI today, anything else
//! later) only needs these types:
//!
//! - [`adb::list_devices`] / [`adb::install_apk`] / [`info::device_info`] — blocking calls, run
//!   them off the UI thread.
//! - [`mirror::Mirror`] — a running screen-mirroring session. It decodes video on its own
//!   thread; the frontend polls [`mirror::Mirror::take_frame`] and [`mirror::Mirror::status`],
//!   and sends touches/keys through [`mirror::Mirror::controller`].

pub mod adb;
pub mod agent;
pub mod control;
pub mod info;
pub mod media;
pub mod mirror;

pub use adb::{Device, DeviceState};
pub use control::{Controller, KeyAction, TouchAction};
pub use info::{DeviceInfo, device_info};
pub use mirror::{Frame, Mirror, MirrorConfig, MirrorStatus, PixelFormat};
