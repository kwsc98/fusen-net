// SPDX-License-Identifier: Apache-2.0 OR MIT

use bytes::Bytes;
use ipnet::Ipv4Net;

use super::{
    ROUTE_COMMAND_TIMEOUT, ROUTE_INSTALL_COMMAND_TIMEOUT, TunError, command::run_route_command,
};

pub(super) const PACKET_OVERHEAD: usize = 0;

pub(super) fn configure_device(configuration: &mut ::tun::Configuration) -> Result<(), TunError> {
    let executable = std::env::current_exe().map_err(TunError::Native)?;
    let executable_dir = executable.parent().ok_or_else(|| {
        TunError::InvalidConfiguration(
            "cannot resolve the stellaris executable directory".to_owned(),
        )
    })?;
    let mut wintun = executable_dir.join("wintun.dll");
    if !wintun.is_file()
        && executable_dir.file_name() == Some(std::ffi::OsStr::new("deps"))
        && let Some(profile_dir) = executable_dir.parent()
    {
        wintun = profile_dir.join("wintun.dll");
    }
    configuration.platform_config(move |platform| {
        platform.wintun_file(wintun.into_os_string());
    });
    Ok(())
}

pub(super) fn decode_packet(packet: &[u8]) -> Result<Bytes, TunError> {
    Ok(Bytes::copy_from_slice(packet))
}

pub(super) const fn encode_packet(packet: Bytes) -> Bytes {
    packet
}

pub(super) async fn install_route(overlay: Ipv4Net, interface: &str) -> Result<(), TunError> {
    let overlay = overlay.to_string();
    let script = "& { param($prefix, $alias) New-NetRoute -DestinationPrefix $prefix -InterfaceAlias $alias -PolicyStore ActiveStore -ErrorAction Stop }";
    run_route_command(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
            &overlay,
            interface,
        ],
        ROUTE_INSTALL_COMMAND_TIMEOUT,
    )
    .await
}

pub(super) async fn remove_route(overlay: Ipv4Net, interface: &str) -> Result<(), TunError> {
    let overlay = overlay.to_string();
    let script = "& { param($prefix, $alias) Remove-NetRoute -DestinationPrefix $prefix -InterfaceAlias $alias -PolicyStore ActiveStore -Confirm:$false -ErrorAction Stop }";
    run_route_command(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
            &overlay,
            interface,
        ],
        ROUTE_COMMAND_TIMEOUT,
    )
    .await
}
