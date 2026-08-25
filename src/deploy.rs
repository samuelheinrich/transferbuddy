//! What transferbuddy is allowed to type on a Cisco device.
//!
//! This module is pure policy — no sockets, no state. It is the one place
//! that decides what may be sent, and [`crate::switch`] runs nothing past it.
//! transferbuddy types six kinds of line on a device: `terminal length 0`,
//! `enable`, `show version`, `dir <device>:`, one `copy <url> <device>:` and
//! `exit` — plus the answers `<Enter>` and `n` to the prompts `copy` asks.
//! A configuration command, a `reload`, a `write`, a `delete` or a copy into
//! `running-config` cannot pass [`vet`]. See [`check_command`] and
//! [`check_destination`].

use anyhow::{bail, Result};

/// Exact commands that need no arguments. `copy` and `dir` take one and are
/// checked separately.
const ALLOWED_COMMANDS: &[&str] = &["terminal length 0", "enable", "exit", "show version", "dir"];

/// URL schemes a `copy` source may use — all of them served by transferbuddy.
const ALLOWED_SCHEMES: &[&str] =
    &["ftp://", "http://", "https://", "scp://", "sftp://", "tftp://"];

/// Destinations that would change the device's configuration or state rather
/// than write a file. Matched anywhere in the destination.
const FORBIDDEN_DESTINATIONS: &[&str] = &[
    "running-config",
    "startup-config",
    "vlan.dat",
    "nvram:",
    "system:",
    "tmpsys:",
    "null:",
    "cns:",
    "pram:",
    "rcp:",
    "archive:",
];

/// Storage devices a file may be copied to.
const ALLOWED_DEVICES: &[&str] = &[
    "flash",
    "bootflash",
    "usbflash",
    "disk",
    "slot",
    "crashinfo",
    "harddisk",
    "webui",
    "sd",
    "msata",
    "obfl",
    "stby-flash",
    "stby-bootflash",
    "stby-usbflash",
    "stby-disk",
    "stby-harddisk",
];

/// What kind of line is being written to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// A whitelisted EXEC command.
    Command,
    /// An answer to a prompt: Enter (accept the default) or `n` (decline).
    Answer,
    /// A password. Never echoed into the transcript or the log.
    Secret,
}

/// The single choke point: nothing reaches the device without passing here.
pub fn vet(text: &str, wire: Wire) -> Result<(), String> {
    if text.contains('\n') || text.contains('\r') {
        return Err("refused: line breaks are not allowed in a single line".into());
    }
    match wire {
        Wire::Command => check_command(text),
        Wire::Answer => {
            if text.is_empty() || text == "n" {
                Ok(())
            } else {
                Err(format!("refused: {text:?} is not an allowed prompt answer"))
            }
        }
        // Only ever sent in reply to a `Password:` prompt, where IOS reads a
        // secret and not a command. The line-break check above still applies.
        Wire::Secret => Ok(()),
    }
}

/// Whitelist for EXEC commands. Anything not listed here is refused — that
/// includes every configuration command, `reload`, `write`, `erase`,
/// `delete` and `format`.
pub fn check_command(cmd: &str) -> Result<(), String> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Err("refused: empty command".into());
    }
    if cmd.chars().any(|c| c.is_control()) {
        return Err("refused: command contains control characters".into());
    }
    // `?` is IOS context help and would leave the session at a half-typed
    // command; `|` and `;` chain further commands.
    if cmd.contains('?') || cmd.contains('|') || cmd.contains(';') {
        return Err(format!("refused: {cmd:?} contains a forbidden character"));
    }
    if ALLOWED_COMMANDS.contains(&cmd) {
        return Ok(());
    }
    if let Some(rest) = cmd.strip_prefix("copy ") {
        return check_copy(rest);
    }
    // `dir <device>:` only reads a listing — it is how the free flash space
    // is known before a copy is started.
    if let Some(rest) = cmd.strip_prefix("dir ") {
        let device = rest.trim();
        return match device.split_once(':') {
            Some((dev, path)) if path.is_empty() || path == "/" => {
                if device_allowed(dev) {
                    Ok(())
                } else {
                    Err(format!("refused: {dev}: is not a known storage device"))
                }
            }
            _ => Err(format!("refused: {device:?} is not a device to list")),
        };
    }
    Err(format!(
        "refused: {cmd:?} is not a command transferbuddy is allowed to run"
    ))
}

fn check_copy(rest: &str) -> Result<(), String> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 2 {
        return Err("refused: copy takes exactly one source URL and one destination".into());
    }
    let (src, dst) = (parts[0], parts[1]);
    let lower = src.to_ascii_lowercase();
    if !ALLOWED_SCHEMES.iter().any(|s| lower.starts_with(s)) {
        return Err(format!(
            "refused: copy source {src:?} is not a transferbuddy URL"
        ));
    }
    check_destination(dst)
}

/// A copy destination must be a storage device (`flash:`, `bootflash:`,
/// `usbflash0:`, ...). Configuration targets are refused: copying into
/// `running-config` or `startup-config` is a configuration change.
pub fn check_destination(dst: &str) -> Result<(), String> {
    let lower = dst.to_ascii_lowercase();
    if let Some(bad) = FORBIDDEN_DESTINATIONS.iter().find(|b| lower.contains(**b)) {
        return Err(format!(
            "refused: {dst:?} targets {bad} — transferbuddy never writes the device configuration"
        ));
    }
    let Some((device, path)) = lower.split_once(':') else {
        return Err(format!(
            "refused: {dst:?} does not name a device — use e.g. flash:"
        ));
    };
    if !device_allowed(device) {
        return Err(format!("refused: {device}: is not a known storage device"));
    }
    if path.contains(':') || path.contains(' ') {
        return Err(format!("refused: {dst:?} is not a valid destination path"));
    }
    Ok(())
}

/// `flash`, `flash-1`, `usbflash0`, `disk0`, `stby-bootflash`, ...
fn device_allowed(device: &str) -> bool {
    if device.is_empty() || device.contains('/') {
        return false;
    }
    let base = device.trim_end_matches(|c: char| c.is_ascii_digit());
    let base = base.strip_suffix('-').unwrap_or(base);
    ALLOWED_DEVICES.contains(&base)
}

/// What to do about a prompt the device printed while `copy` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    /// Send Enter — accept the default the device offers.
    Accept,
    /// Send `n` — decline.
    Decline,
    /// Something transferbuddy will not answer; the session is torn down.
    Abort(String),
}

/// Decide how to answer one prompt. Only the handful of questions a plain
/// `copy` asks are answered; anything else aborts the session, so a prompt
/// nobody anticipated can never be confirmed by accident.
pub fn classify_prompt(prompt: &str, overwrite: bool) -> PromptAction {
    let p = prompt.trim();
    let lower = p.to_ascii_lowercase();

    // Never erase a filesystem, whatever the wording.
    if lower.contains("erase") || lower.contains("format") || lower.contains("squeeze") {
        return PromptAction::Decline;
    }
    if lower.contains("over write") || lower.contains("overwrite") {
        return if overwrite { PromptAction::Accept } else { PromptAction::Decline };
    }
    for known in [
        "destination filename",
        "source filename",
        "address or name of remote host",
        "source username",
        "source password",
    ] {
        if lower.starts_with(known) {
            return PromptAction::Accept;
        }
    }
    PromptAction::Abort(format!("unexpected prompt from the device: {p:?}"))
}

/// Turn the device's `copy` output into a result. IOS reports success as
/// "N bytes copied in M secs" and failures with a `%`-prefixed line.
pub fn verdict(output: &str) -> Result<String> {
    if let Some(line) = output.lines().find(|l| l.contains("bytes copied in")) {
        return Ok(line.trim().to_string());
    }
    if let Some(err) = output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("%Error") || l.starts_with("%error") || l.contains("Copy aborted"))
    {
        bail!("{err}");
    }
    if let Some(warn) = output.lines().map(str::trim).find(|l| l.starts_with('%')) {
        bail!("{warn}");
    }
    Ok("copy finished (device reported no byte count)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_exactly_the_listed_commands() {
        for cmd in [
            "terminal length 0",
            "enable",
            "exit",
            "show version",
            "dir",
            "dir flash:",
            "dir bootflash:",
            "dir flash-1:",
            "copy http://10.0.0.1:8080/img.bin flash:",
        ] {
            assert!(check_command(cmd).is_ok(), "must be allowed: {cmd}");
        }
    }

    #[test]
    fn read_only_commands_stay_narrow() {
        // Only `show version`, not any other show.
        assert!(check_command("show running-config").is_err());
        assert!(check_command("show version | include foo").is_err());
        assert!(check_command("show tech-support").is_err());
        // `dir` needs a device, and only a known one.
        assert!(check_command("dir nvram:").is_err());
        assert!(check_command("dir system:running-config").is_err());
        assert!(check_command("dir flash:secret/dir").is_err());
        assert!(check_command("dir /all").is_err());
    }

    #[test]
    fn refuses_every_configuration_command() {
        for cmd in [
            "configure terminal",
            "conf t",
            "config-transaction",
            "write memory",
            "write erase",
            "copy running-config startup-config",
            "reload",
            "reload in 5",
            "delete flash:image.bin",
            "erase startup-config",
            "format flash:",
            "archive download-sw",
            "license boot level",
            "show running-config",
            "terminal length 0; configure terminal",
            "enable | configure",
            "copy http://10.0.0.1/x.bin flash: ! and more",
        ] {
            assert!(check_command(cmd).is_err(), "must be refused: {cmd}");
        }
    }

    #[test]
    fn copy_destination_must_be_a_storage_device() {
        for dst in ["flash:", "flash:img.bin", "bootflash:", "flash-1:", "usbflash0:", "disk0:img.bin"] {
            assert!(
                check_command(&format!("copy tftp://10.0.0.1/img.bin {dst}")).is_ok(),
                "must be allowed: {dst}"
            );
        }
        for dst in [
            "running-config",
            "startup-config",
            "system:running-config",
            "nvram:startup-config",
            "null:",
            "tftp://10.0.0.1/x",
            "flash",
            "unknowndev:",
        ] {
            assert!(
                check_command(&format!("copy tftp://10.0.0.1/img.bin {dst}")).is_err(),
                "must be refused: {dst}"
            );
        }
    }

    #[test]
    fn copy_source_must_be_a_transferbuddy_url() {
        assert!(check_command("copy flash:a.bin flash:b.bin").is_err());
        assert!(check_command("copy startup-config tftp://10.0.0.1/cfg").is_err());
        assert!(check_command("copy rcp://10.0.0.1/x.bin flash:").is_err());
    }

    #[test]
    fn answers_are_restricted_to_enter_and_no() {
        assert!(vet("", Wire::Answer).is_ok());
        assert!(vet("n", Wire::Answer).is_ok());
        assert!(vet("y", Wire::Answer).is_err());
        assert!(vet("configure terminal", Wire::Answer).is_err());
        assert!(vet("secret\rconfigure terminal", Wire::Secret).is_err());
        assert!(vet("terminal length 0\nreload", Wire::Command).is_err());
    }

    #[test]
    fn prompts_that_erase_are_always_declined() {
        assert_eq!(
            classify_prompt("Erase flash: before copying? [confirm]", true),
            PromptAction::Decline
        );
        assert_eq!(
            classify_prompt("Format flash: [confirm]", true),
            PromptAction::Decline
        );
    }

    #[test]
    fn overwrite_follows_the_setting() {
        let q = "%Warning:There is a file already existing with this name. \
                 Do you want to over write? [confirm]";
        assert_eq!(classify_prompt(q, false), PromptAction::Decline);
        assert_eq!(classify_prompt(q, true), PromptAction::Accept);
    }

    #[test]
    fn known_copy_prompts_are_accepted_unknown_ones_abort() {
        assert_eq!(
            classify_prompt("Destination filename [img.bin]?", false),
            PromptAction::Accept
        );
        assert_eq!(
            classify_prompt("Address or name of remote host [10.0.0.1]?", false),
            PromptAction::Accept
        );
        assert!(matches!(
            classify_prompt("Proceed with reload? [confirm]", true),
            PromptAction::Abort(_)
        ));
        assert!(matches!(
            classify_prompt("Continue? [yes/no]:", true),
            PromptAction::Abort(_)
        ));
    }

    #[test]
    fn verdict_reads_the_device_output() {
        assert!(verdict("...\n521510912 bytes copied in 231.402 secs\ncat9k#").is_ok());
        let e = verdict("%Error opening tftp://10.0.0.1/x.bin (Timed out)\n").unwrap_err();
        assert!(e.to_string().contains("Timed out"));
        assert!(verdict("Copy aborted.\n").is_err());
    }
}
