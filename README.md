# TransferBuddy

A native macOS desktop app and CLI/TUI file transfer server for provisioning **Cisco** switches,
routers, wireless controllers and similar network devices. Point it at a
directory and it serves the files over **FTP, HTTP, HTTPS, SCP, SFTP and TFTP**
simultaneously. Both frontends share transfer, SSH and Cisco INSTALL upgrade
logic through the same Rust core, with live status and ready-to-paste `copy`
commands. Each running process owns its connections and jobs.

**Current release: 0.2.14 · macOS 13+ · Apple Silicon and Intel.**

| macOS | Desktop app | CLI / TUI |
| --- | --- | --- |
| Apple Silicon (arm64) | [Download DMG](https://github.com/samuelheinrich/transferbuddy/releases/download/v0.2.14/TransferBuddy-0.2.14-aarch64-apple-darwin.dmg) | [Download CLI](https://github.com/samuelheinrich/transferbuddy/releases/download/v0.2.14/transferbuddy-0.2.14-aarch64-apple-darwin.tar.gz) |
| Intel (x86_64) | [Download DMG](https://github.com/samuelheinrich/transferbuddy/releases/download/v0.2.14/TransferBuddy-0.2.14-x86_64-apple-darwin.dmg) | [Download CLI](https://github.com/samuelheinrich/transferbuddy/releases/download/v0.2.14/transferbuddy-0.2.14-x86_64-apple-darwin.tar.gz) |

[Release notes and checksums](https://github.com/samuelheinrich/transferbuddy/releases/tag/v0.2.14)
· [Desktop architecture](docs/desktop.md)
· [Screenshot gallery](docs/screenshots/README.md)
· [Roadmap](ROADMAP.md)

Windows desktop support is a deferred proposal, open for contributions with no
target release. See the [roadmap](ROADMAP.md) for scope, prerequisites, tests,
limitations and effort estimates.

![TransferBuddy desktop Dashboard](docs/screenshots/desktop/dashboard.png)

## Why

Copying an IOS-XE image to a switch usually means setting up a TFTP or HTTP
server by hand, figuring out your local IP, and typing the copy URL from
memory. transferbuddy does all of that: it serves the current directory,
detects a sensible local address, and generates the exact
`copy http://…/image.bin flash:` command for every enabled protocol — ready to
copy with a single keypress.

## Features

- **Six protocols, one process:** FTP, HTTP, HTTPS, SCP, SFTP and TFTP over one
  shared directory, each individually startable.
- **Ready-to-paste Cisco commands** for every enabled protocol, with the right
  IP, port and credentials — one keypress to the clipboard.
- **You pick the address:** on a laptop on cable *and* Wi-Fi, `i` decides which
  interface ends up in the generated URLs, and the choice is remembered.
- **Central SSH connections:** add or discover devices in Connect, reuse their
  sessions in Transfer and Upgrade, reconnect with in-memory credentials and
  open an interactive CLI for your own commands.
- **Install upgrades:** copy, compare device/local MD5, save configuration,
  confirm reload (or choose YOLO), reconnect and verify every stack member.
- **Switch sessions that stay open:** a session survives the copy, keeps
  reading free flash space, IOS version and stack members, and is watched by a
  ping monitor — the groundwork for tracking an install across a reload.
- **Live session view:** progress, current and average speed, ETA, per-protocol
  counters.
- **Fixed, known credentials** (`cisco` / `cisco123`) — nothing random to look
  up at a switch console.
- **Safe by default:** downloads only, nothing auto-started, path traversal and
  symlink escapes blocked, per-IP lockout after failed logins.
- **Integrity checks:** MD5/SHA-256/SHA-512 of any file, with paste-and-compare.
- **Self-contained:** self-signed TLS certificate and SSH host key are
  generated on first use; no runtime dependencies and no telemetry.

## Installation

### Desktop app

Download the DMG for your Mac from the table above, open it, and drag
**TransferBuddy.app** to **Applications**. Choose a folder on first launch;
services stay stopped until you start them. The app bundle also contains the
CLI and the optional standard-port helper.

The public binaries are ad-hoc signed and are **not Apple-notarized**. If macOS
blocks first launch, use **System Settings → Privacy & Security → Open Anyway**
after checking the download source. On macOS 15+, allow TransferBuddy under
**Privacy & Security → Local Network** for switch access. Ad-hoc builds cannot
use the Developer ID authenticated port helper; choose higher ports if a normal
bind is denied. See [packaging and distribution](docs/desktop.md#packaging-and-distribution).

### CLI / TUI binary

Download the CLI archive for your architecture and extract it. From the
extracted directory:

```bash
sudo install -m 755 transferbuddy /usr/local/bin/transferbuddy
transferbuddy --version             # transferbuddy 0.2.14
cd ~/ios-images
transferbuddy
```

Alternatively, install into `~/.local/bin` if it is on your `PATH`.
`SHA256SUMS.txt` on the release page covers all four downloads. With the checksum
file and the corresponding downloads in one folder, verify them with
`shasum -a 256 --check --ignore-missing SHA256SUMS.txt`.

### Homebrew from this repository

There is no published Homebrew tap yet. To build the current CLI from `main`,
copy the included head-only formula into a local tap:

```bash
brew tap-new local/transferbuddy
cp Formula/transferbuddy.rb "$(brew --repository local/transferbuddy)/Formula/transferbuddy.rb"
brew install --HEAD local/transferbuddy/transferbuddy
```

### System-wide (`/usr/local/bin`)

Build the binary first, then copy it into `/usr/local/bin` so `transferbuddy`
is available to every user and every shell, independent of where the repository
lives. For the current architecture only:

```bash
cargo build --release
sudo install -m 755 target/release/transferbuddy /usr/local/bin/transferbuddy
```

For a universal binary that runs on both Apple Silicon and Intel — see
[Release builds](#release-builds) for the full recipe:

```bash
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
mkdir -p dist && lipo -create -output dist/transferbuddy \
  target/aarch64-apple-darwin/release/transferbuddy \
  target/x86_64-apple-darwin/release/transferbuddy
sudo install -m 755 dist/transferbuddy /usr/local/bin/transferbuddy
```

Verify the installation:

```bash
which transferbuddy      # → /usr/local/bin/transferbuddy
transferbuddy --version
```

Notes:

- `sudo` prompts for a password, so run the install command in a real terminal
  — it fails in editor consoles and other non-interactive shells.
- Use `install` (or `cp`), not a symlink. A symlink into the repository ties
  the installed command to that checkout, which breaks if the directory is
  moved, deleted or lives on a cloud-synced volume that is not mounted.
- No `sudo`? `~/.local/bin` works the same way if it is on your `PATH`.
- The installed file is a snapshot — re-run the `install` command after every
  release build.

### From source

Requires a Rust toolchain (`rustup`, stable):

Release builds and CI use **Rust 1.94.0** so formatting, lint checks and binaries
use the same compiler. If building with another installed toolchain, use
`cargo +1.94.0` to reproduce that environment.

```bash
cargo install --path .
# or:
cargo build --release        # binary in target/release/transferbuddy
```

Native builds work on both Apple Silicon (arm64) and Intel (x86_64) Macs; see
[Release builds](#release-builds) for the separate architecture builds.

## Quick start

```bash
cd ~/ios-images
transferbuddy              # serves the current directory, opens the TUI
```

Common variants:

```bash
transferbuddy --root ./images            # serve another directory
transferbuddy --http --port-http 8080    # only HTTP, explicit port
transferbuddy --http --https --tftp      # several protocols at once
transferbuddy --all                      # everything: FTP/HTTP/HTTPS/SSH/TFTP
transferbuddy --all --no-tui             # headless: structured logs on stdout
sudo transferbuddy --all                 # privileged ports (80/443/21/22/69)
transferbuddy --sftp --sftp-port 2222 --username cisco
transferbuddy --bind 192.168.1.10 --http # bind one specific interface
```

The TUI opens on an intro screen — press `Enter` (or Space/Esc) to start it.

**Nothing listens until you start it.** In the TUI every service comes up
*stopped*, no matter what the config or the CLI flags say: the flags only
select which services are *enabled*. Start them in Dashboard with `s`
(selected service) or `S` (all enabled ones). Headless mode (`--no-tui`) has no
such control and starts the enabled services immediately.

Press `h` or `?` inside the TUI for the key reference. In the file browser,
press `Enter` on a file to transfer it. Press `y` to preview Cisco copy
commands, then `y` in that popup to copy one to the clipboard.

## Desktop app

The native Rust desktop app and the terminal app share `transferbuddy-core`.
Both offer Dashboard, Connect, Transfer, Upgrade and Logs. They run independently;
they share saved settings, while SSH sessions, credentials and jobs belong to the
process in which they were created.

The desktop uses the TUI's horizontal **1 Dashboard · 2 Connect · 3 Transfer ·
4 Upgrade · 5 Logs** navigation. Its console theme combines embedded JetBrains
Mono, amber controls, cyan focus and semantic status colors. **Cmd/Ctrl+1–5** switches
views; **Cmd/Ctrl+K** opens the command palette. Settings offer Dark/Light/System,
Standard/Compact density, an interface scale slider and reduced motion. The
default is **Compact at 0.85 scale**. Pane sizes, column widths and display
preferences are remembered. Dashboard groups service selection, Start/Stop and
Options together, with the copy-URL interface selector beside them; plain status
counts sit in the header.

Open CLI consoles for several devices at once. Each console is a movable,
resizable black window inside TransferBuddy, with a tab in the bottom taskbar.
**Minimize**, **Escape**, or clicking the exposed background keeps the SSH CLI
connected; click its tab to restore it. **Close** ends that CLI channel. Select
text and use Cmd/Ctrl+C to copy; Cmd/Ctrl+V or **Paste** sends clipboard text to
the focused device. Ctrl+C without a copy selection interrupts the device.
SSH login and enable passwords are masked. Credentials embedded in copy URLs
remain visible, including in copy-command logs. GUI text fields support
Cmd/Ctrl+A, copy and paste; macOS Control+A/C/V also work outside the CLI.
Info links to the project and credits Samuel Heinrich; Help explains each view,
keyboard workflows, interfaces and SSH troubleshooting.

```bash
cargo run -p transferbuddy-desktop
cargo run -p transferbuddy-desktop -- --root ./images
cargo run -p transferbuddy-desktop -- --config /path/to/config.toml
```

On the first launch, choose a local folder. The desktop remembers it separately
from the TUI, which continues to use its working directory or `--root`. Services
remain stopped until started. Changing the root stops services, clears file
assignments and verification, and is blocked while jobs are active. In the TUI,
use `o` in Dashboard to change it.

Close the desktop window to keep jobs running. The menu bar icon and
**Show TransferBuddy** reopen it; a second launch with the same configuration
also reopens the existing window. **Quit TransferBuddy** asks before closing
active jobs. Transfers and upgrades hold a macOS activity token to prevent
idle sleep. SSH passwords stay in memory and survive reconnects in that process.

Transfer has a device list and resizable local/remote panes. Double-click or
Enter opens directories or file details. Use **Copy** or Cmd/Ctrl+Enter to queue
selected files; focus a device and press Enter to
choose its protocol. Active protocols appear in SFTP, HTTPS, FTP, HTTP priority
order. The choice is remembered for that device. Reverse transfers use FTP.
Cmd/Ctrl-click toggles files; Shift-click selects a range. Cmd/Ctrl+J opens
Jobs, with progress, cancel, retry and reconnect/resume (`J` in the TUI). Jobs freeze paths and
protocols and run in order per device, concurrently across devices.
Remote deletion and upgrades require manually typing lowercase `y` and then
clicking **Confirm**; the device
identity and target are shown before approval. ESC leaves interactive CLI;
Ctrl+C goes to the switch. Upgrade rows show metadata, progress, speed and ETA,
with upload-and-verify, INSTALL, YOLO and remove-inactive actions.

On macOS 15+, allow **TransferBuddy** under **System Settings → Privacy &
Security → Local Network**. Terminal and the desktop app have separate network
permissions. If a host works in Terminal but the app reports `No route to host
(os error 65)`, check this permission and the network/VPN, then reconnect.
Connect and Settings provide an **Open Local Network settings** button.
See [Apple's local network settings guide](https://support.apple.com/en-gb/guide/mac-help/mchla4f49138/mac).

The shared SSH client already offers `diffie-hellman-group14-sha1`, `ssh-rsa`,
AES CTR/CBC, 3DES CBC and HMAC-SHA1 after its modern algorithms. An OpenSSH
`no matching key exchange` error therefore does not establish that TransferBuddy
has the same problem. Negotiation failures now identify the algorithm category
and the switch's offered algorithms separately from TCP/login failures.

For unset desktop service ports, the defaults are TCP 21/22/80/443 and UDP 69.
Saved port choices are preserved. Every listener attempts a normal bind first,
so low ports are not rejected merely because the process is unprivileged.
On macOS, a specific interface address may require the standard-port helper.
Enable it in Desktop Settings after installing the signed `.app`; macOS asks
for system approval where necessary. The helper authenticates the app's audit
token and Developer ID Team ID and passes only bound socket descriptors.
It cannot access transfer files, run commands or receive SSH credentials.
Unsigned development builds use direct binds or higher ports.

Build an installer on macOS with Xcode Command Line Tools and Python 3.11+:

```bash
cargo install cargo-packager --version 0.11.8 --locked
python3 scripts/package-macos.py
# dist/macos-native/TransferBuddy.app and TransferBuddy-<version>-native.dmg
```

Apple Silicon and Intel packaging targets are supported by the script and the
manual CI installer workflow. For signing/notarization, see
[desktop architecture and release checks](docs/desktop.md). Windows/Linux GUI
packaging remains future work; the CLI and shared engine remain independently
buildable without desktop libraries.

## The TUI

A keyboard-driven Atari-8bit-flavoured terminal UI: dark CRT background, amber
titles, phosphor-green highlights. Five views, switched with `1`–`5`; `Tab` selects a pane within a view, and the current view's keys are always listed in the footer.

### Intro

Every start opens with a short block-letter animation: the logo wipes in, the
classic rainbow bars scroll through it, the subtitle types itself out and a
loading bar fills up. Any key fast-forwards to the end.

![transferbuddy intro animation](docs/screenshots/tui/intro-anim.svg)

The screen then *stays* until you press `Enter`, `Space` or `Esc` — nothing
starts behind your back, and you get to look at it as long as you like. Skip
the whole thing with `--no-intro` (or `intro = false` in `config.toml`).

![transferbuddy intro, ready to start](docs/screenshots/tui/intro-ready.svg)

### 1 · Dashboard

![TransferBuddy TUI dashboard](docs/screenshots/tui/dashboard.svg)

Shared root, advertised address, service controls and the live file-transfer
monitor. `Tab` changes focus between services and transfers. `Space` enables a
service, `s` starts/stops it, `S` starts enabled services, and `X` stops all.
`Enter` edits port, bind address, credentials and upload settings. The detail
panel shows the TLS certificate or SSH host-key fingerprint when relevant.
`i` changes the address used in copy URLs. All six file protocols remain
available; SCP/SFTP share one SSH file service.

### 2 · Connect

![TransferBuddy TUI connect](docs/screenshots/tui/connect.svg)

Manage the SSH sessions used by Transfer and Upgrade here. `a` adds a device;
`b` imports addresses separated by spaces, commas, semicolons or pasted
newlines. `Enter` advances through SSH port, username, password and optional
enable password, then submits on the final button. No transfer protocol is
required. Device credentials stay in memory until the application exits.

The Bulk **device IP / Subnet:** field accepts individual IPs, IPv4 CIDRs
(`/16` through `/32`), or both, for example
`192.168.22.0/24, 192.168.11.11`. Input is validated before connecting, and
overlapping targets are deduplicated. Explicit IPs connect directly; subnet
discovery attempts SSH only on pingable hosts. Up to 32 pings and 16 SSH
connects run concurrently. Failed discovered SSH connections disappear from
the visible list, with reasons retained in Logs. `S` cancels scanning. Device connections automatically accept and store unknown and changed SSH host
keys by default. Disable **Automatically accept device host keys** in Settings
to ask for unknown keys and refuse changed keys, including bulk/subnet connections.

The table shows hostname, address, model, INSTALL/BUNDLE mode, IOS version,
free flash and SSH state. Details include storage totals, image, uptime and
ping. `Enter` opens the transcript. `r` refreshes facts or reconnects an
expired session using its original credentials. During a copy, refresh uses a
second SSH session. `x` disconnects and `X` clears finished rows.

`c` opens **Connect to CLI**: type commands directly, including configuration
commands. Enter, Tab, arrows and Ctrl-C are forwarded to the device;
**Esc** leaves the CLI and returns to TransferBuddy. Ending the remote shell
with `exit` also returns automatically. This CLI uses a separate shell,
preserving the automation session. SSH keepalives detect dead sessions.

### 3 · Transfer

![TransferBuddy TUI transfer](docs/screenshots/tui/transfer.svg)

The device overview displays about ten devices on a normal-sized terminal.
When there are more, its title shows the visible range and **↑↓ scroll**, plus
indicators for additional rows above/below. `Tab` selects devices, local files
(left), or remote storage (right). `Left` / `Right` select the file panes.

`Enter` opens a directory or copies a file in either pane. Select `..` or
press `Backspace` to return to the
parent; `Home` returns to the root. Local browsing stays within the shared
root (the startup directory by default), including symbolic links. `s` sorts,
`/` filters, `R` refreshes and `H` computes MD5/SHA-256/SHA-512 for comparison.
IOS `.bin`, `.pkg`, `.conf` files and `packages.conf` are highlighted in amber.

`t` sends a selected local file to the selected remote directory. On the first
transfer, a protocol picker lists active services in the order **SFTP, HTTPS,
FTP, HTTP**, followed by SCP and TFTP. If none are running, it offers enabled
services, or all protocols if none are enabled. Enter accepts the suggestion.
The selected protocol is remembered per device. `Enter` on a device in the
overview or `p` opens the picker to change it. A stopped service or failed transfer lets you choose
again. Services start automatically when a transfer requires them.

`t` on a remote file receives it in the current local directory using
`copy flash:… ftp://…`; receiving currently uses FTP. Sending supports FTP,
HTTP, HTTPS, SCP, SFTP and TFTP as supported by the device. Any file type is
accepted. Existing destination files are preserved. A receive authorizes one
exact destination from the selected device, even with general uploads off;
ordinary uploads retain their configured directory/policy and size limits.

`y` on a local file shows copy commands for enabled protocols; `y` in that
popup copies a command to the clipboard. Connections are managed in Connect;
transfers share those connections.

In the remote pane, **Del** or **D** opens a red deletion warning for the
selected file or directory. **Only plain lowercase `y` deletes; every other
key cancels.** Directories are removed with all their files and subdirectories
using `delete /force /recursive filesystem:/path`; files use
`delete /force filesystem:/path`. The browser refreshes afterward. Deletion
cannot be undone. See the [Cisco flash file system guide](https://www.cisco.com/c/en/us/td/docs/switches/lan/catalyst9200/software/release/16-11/configuration_guide/sys_mgmt/b_1611_sys_mgmt_9200_cg/working_with_the_flash_file_system.html).

#### Upgrade platform warning

The Upgrade tab checks the selected filename against the model reported by
`show version`, including stack members. A mismatch displays **plattform
mismatch** before sending and in the console. The transfer remains allowed.
Legacy devices such as 2960X, 3550 and 3850, and unrecognized models, are skipped.
The Transfer tab accepts arbitrary files without this image warning.

| Platform | Accepted image family (`*.bin`, including SMUs) |
|---|---|
| C9200 / C9200L / C9200CX | `cat9k_lite_iosxe.*`, `cat9k_lite_iosxe_npe.*` |
| C9300 / C9400 / C9500 / C9600 and their variants | `cat9k_iosxe.*`, `cat9k_iosxe_npe.*` |
| C9800-40 | `C9800-40-universalk9[_wlc].*`, shared `C9800-universalk9_wlc.*` |
| C9800-80 | `C9800-80-universalk9[_wlc].*`, shared `C9800-universalk9_wlc.*` |
| C9800-L | `C9800-L-universalk9[_wlc].*` |
| C9800-CL | `C9800-CL-universalk9.*` |

Cisco sources: [9200 release notes](https://www.cisco.com/c/en/us/td/docs/switches/lan/catalyst9200/software/release/17-15/release_notes/ol-17-15-9200.html),
[Catalyst 9000 upgrade guide](https://www.cisco.com/c/en/us/support/docs/switches/catalyst-9300-series-switches/216231-upgrade-guide-for-cisco-catalyst-9000-sw.html),
[9500 release notes](https://www.cisco.com/c/en/us/td/docs/switches/lan/catalyst9500/software/release/17-18/release_notes/ol-17-18-9500.html),
[9200 SMU guide](https://www.cisco.com/c/en/us/td/docs/switches/lan/catalyst9200/software/release/16-11/configuration_guide/sys_mgmt/b_1611_sys_mgmt_9200_cg/software_maintenance_upgrade.html),
[9800 image mapping](https://www.cisco.com/c/en/us/support/docs/wireless/wireless-lan-controller-software/222654-download-cisco-ios-xe-17-12-4-esw-image.pdf)
and [9800-40 upgrade example](https://www.cisco.com/c/en/us/support/docs/wireless/catalyst-9800-series-wireless-controllers/215550-hitless-software-upgrade-on-catalyst-980.pdf).
Researched on 2026-10-02. The check compares filename families, not image contents
or supported software releases. For example, `cat9k_lite_iosxe.*.smu.bin` still
belongs to the 9200 family even if stored in a directory named `9300/smu`.

### 4 · Upgrade

![TransferBuddy TUI upgrade](docs/screenshots/tui/upgrade.svg)

The upper pane is the local file browser and selected transfer protocol; the
lower pane holds one job per connected device. `Tab` switches between them.
Choose a file with Enter, then use `a` to assign it to the selected device or
`A` to assign it to every device. Different devices may have different images.
`p` selects the copy protocol. New SSH sessions are added in Connect.

Each device row shows model, running version, free flash, upload percentage,
speed and ETA. Use its action buttons or press Enter on the device for all
actions. The buttons can also be clicked in a sufficiently wide terminal.

`d` copies the assigned file, computes its local MD5 and runs
`verify /md5 flash:filename.bin` on the device. If the image already exists,
it verifies it without copying again. `V` verifies an existing image directly,
including one copied during an earlier application session, without starting
a transfer service. Installation becomes available only when the image is
present, its hash matches the local file, and its version differs from the
running version. Changing the assigned file requires fresh verification; the
device hash and current version are also checked again before installation.

`u` or the device’s **Upgrade** button opens the install action. **Only plain
lowercase `y` starts it; every other key cancels.** After confirmation,
TransferBuddy verifies
existing INSTALL mode, a `packages.conf` boot variable and disabled manual
boot. Correct incompatible boot settings through Connect CLI first; automatic
BUNDLE conversion is not performed. The app runs `write memory`, requires its
success acknowledgement, then sends:

```text
install add file flash:filename.bin activate commit
```

The actual switch reload question is displayed in the console. **Only plain
lowercase `y` confirms; other keys decline.** Closing a transcript before the
question arrives does not approve it; reopen it with `v` to answer. No ordinary
install reload is silently confirmed.

`Y` selects **YOLO**. Its separate confirmation explains the automatic reboot,
after which the app saves configuration and sends:

```text
install add file flash:filename.bin activate commit prompt-level none
```

After reload starts, an elapsed timer is green below five minutes, amber from
five through ten, and red from ten minutes onward. TransferBuddy pings the
switch and retries SSH/login once reachable, including failures while SSH or
RADIUS is still starting. Saved credentials are reused. `show version` verifies
the requested release and every reported stack member in INSTALL mode. Old
software answering just before the actual restart is briefly retried. The
job reports success only after the version check passes.

`i` offers `install remove inactive` on an idle device, also available in its
actions menu. A red **Remove inactive** button appears beside the device when
the selected image exceeds available flash. The actual removal list
is displayed, with a warning if it includes a running member's release or
active image. Only lowercase `y` approves deletion; every other key declines.
Storage/facts are refreshed afterward. Transfers and installs can run on
several devices; confirmations and progress remain per device.

Implementation references: [Cisco 9200 upgrade procedure](https://www.cisco.com/c/en/us/td/docs/switches/lan/catalyst9200/software/release/17-11/release_notes/ol-17-11-9200/upgrading_the_switch_software.html)
and [Cisco install command reference](https://team-development.cisco.com/c/en/us/td/docs/switches/lan/catalyst9200/software/release/17-17/command_reference/b_1717_9200_cr/system_management_commands.html).
The automated installer handles full release `.bin` images; SMUs and extracted
packages may be transferred but are not treated as full release upgrades.

### 5 · Logs

![TransferBuddy TUI logs](docs/screenshots/tui/logs.svg)

Live structured service, connection, transfer and upgrade logs. `↑↓` scrolls,
`PgUp`/`PgDn` moves a page, `G` follows the tail, `/` filters text, `L` sets the
minimum level and `P` filters by protocol. Failed subnet connections and
transient upgrade reconnect errors are retained here.

### Help

![TransferBuddy TUI Help](docs/screenshots/tui/help.svg)

`h` or `?` opens the key reference — two tables, one key per line, keys
highlighted. On short terminals it scrolls with `↑`/`↓`.

### Sound

Toggling settings gives short "PC speaker" style blips — one tone for on, a
different one for off, a third for a rejected value. `m` mutes them (the
indicator top right switches from `♪` to `×`), as does `--no-sound`. The
setting is remembered.

## Which address ends up in the URLs

A listener bound to `0.0.0.0` answers on every interface, but each copy URL
contains one address. For a selected device, **Automatic** uses the local
address chosen by the route to that device. Without a device, general copy
previews use the automatically detected local address.

In the desktop, **Links** beside Settings stays visible in every view:

- **Automatic:** shows the effective interface/IP for the selected device and
  protocol. Without a selection it shows `Auto · per device`.
- **Local interface:** pin a name such as `en0`; its current address follows DHCP.
- **Manual interface/IP:** enter an interface name or an address assigned locally.
- **Fixed service bind:** takes precedence over the global selection. Different
  protocol binds show `Multiple IPs`, with their mapping in the menu.
- **Copy IP:** copies the resolved address for the corresponding protocol.

![Global Copy URL interface selector](docs/screenshots/desktop/interfaces.png)

In the TUI, `i` cycles interfaces in Dashboard and the copy-command popup.
The choice is saved as `advertise = "en12"` in `config.toml`. CLI equivalents:

```bash
transferbuddy --http --interface en12
transferbuddy --http --interface 10.41.10.108
transferbuddy --http --no-tui
```

A disappeared interface or address is **Unavailable**; a transfer does not
silently fall back to another network. Correct the selection or service bind
before retrying. Running copies keep their original command. New transfers and
explicit retries resolve the latest interface/IP; **Retry with interface…**
lets you correct it while retrying. Logs retain the actual copy command and the
GUI highlights its server IP.

## Ports and sudo

transferbuddy detects whether it runs with root privileges and picks the
default ports accordingly. It never invokes `sudo` itself.

| Service   | with sudo | without sudo |
|-----------|-----------|--------------|
| FTP       | 21        | 2121         |
| HTTP      | 80        | 8080         |
| HTTPS     | 443       | 8443         |
| SFTP/SCP  | 22        | 2222         |
| TFTP      | 69        | 6969         |

These are CLI defaults. The desktop defaults to standard ports unless saved
settings override them. A listener always tries a normal bind first; when the OS
refuses it, Logs explain the failure. The optional macOS helper requires a
Developer ID signed app. Public ad-hoc builds should use a higher port when
needed. Nothing silently starts on a different port.

> **TFTP note:** Cisco IOS cannot specify a TFTP port; `copy tftp://…` always
> uses 69. To serve classic TFTP to IOS devices, run with `sudo` (or use HTTP,
> which every modern IOS-XE supports and which is faster).

Ports and bind addresses can be changed in Dashboard service options or
via CLI flags (`--port-http`, `--port-https`, `--port-ftp`, `--port-sftp`,
`--port-tftp`, `--bind`).

## Protocols

| Protocol | Encrypted | Auth               | Uploads | Cisco example |
|----------|-----------|--------------------|---------|---------------|
| HTTP     | no        | none (download)    | opt-in  | `copy http://192.168.1.10:8080/img.bin flash:` |
| HTTPS    | yes (TLS) | none (download)    | opt-in  | `copy https://192.168.1.10:8443/img.bin flash:` |
| FTP      | no        | user/password      | opt-in  | `copy ftp://user:pass@192.168.1.10:2121/img.bin flash:` |
| SFTP     | yes (SSH) | user/password      | opt-in  | `copy sftp://user:pass@192.168.1.10:2222/img.bin flash:` |
| SCP      | yes (SSH) | user/password      | opt-in  | `copy scp://user:pass@192.168.1.10:2222/img.bin flash:` |
| TFTP     | no        | none               | opt-in  | `copy tftp://192.168.1.10/img.bin flash:` |

SFTP and SCP share one SSH service (one port, one host key, one user account).
FTP, HTTP and TFTP are clearly marked **CLEARTEXT** in the TUI — anyone on the
network path can read transferred data and FTP credentials.

## Authentication

- HTTP, HTTPS and TFTP run without authentication as download-only services by
  default.
- FTP, SFTP and SCP require a username and password. The default credentials
  are **`cisco` / `cisco123`** — nothing is ever generated randomly, so an
  unattended start always uses the same, known pair.
- A different password (set with `--password`, in the TUI with `w`, or in the
  service edit popup) is stored with `0600` permissions under
  `state/secrets.toml`. SSH login and enable fields are masked. Copy URLs and
  copy-command logs intentionally retain transfer-service credentials so the
  generated command can be reused. Passwords are not written to `config.toml`.
- Repeated failed logins lock out the source IP temporarily (default: 5
  failures, 60 s).

## Uploads

Downloads only by default. Enable uploads explicitly with `--uploads` or the
`u` key in the TUI (asks for confirmation). When enabled:

- uploads land in a configurable directory (`--upload-dir`, relative to root),
- existing files are never overwritten (configurable),
- file names are validated (no path separators, no hidden/dot files),
- data is written to a unique `.name.part-XXXX` temp file and only renamed to
  its final name after a successful transfer,
- free disk space is checked before accepting the transfer,
- a size limit can be set with `--max-upload-mib`.

## Security

- **Path traversal:** every client path is normalized and canonicalized; `..`,
  absolute paths and encoded variants are rejected before touching the
  filesystem.
- **Symlinks:** links that resolve to targets outside the shared root are
  blocked (links within the root work).
- **Credentials:** SSH device passwords stay in memory. Transfer-service
  passwords are saved in a `0600` secrets file, outside `config.toml`. Copy URLs
  and command logs intentionally show service credentials. TLS private keys
  and SSH server keys are stored with `0600` permissions.
- **Device host keys:** automatically accepted and stored by default. Disable
  this in Desktop Settings, or set `auto_accept_host_keys = false` in
  `config.toml`, to ask about unknown keys and reject changed keys.
- **Rate limiting:** per-IP lockout after repeated failed logins.
- **Limits & timeouts:** configurable max parallel sessions, idle session
  timeouts, upload size limit.
- **Warnings:** binding to all interfaces and enabling cleartext protocols is
  called out in the logs and the TUI.
- **No telemetry.** Outbound connections are used only for requested switch SSH sessions and ping checks.

## HTTPS certificates

On first HTTPS start a self-signed certificate is generated (valid for
`localhost` and all current local IPs) and stored under
`certificates/cert.pem` / `key.pem`. The Dashboard service detail shows the SHA-256
fingerprint so you can verify it from the device side.

To use your own certificate, set in `config.toml`:

```toml
[tls]
cert_path = "/path/to/cert.pem"
key_path  = "/path/to/key.pem"
```

Many IOS versions require the server certificate (or its CA) to be trusted
before `copy https:` works; either install the certificate as a trustpoint or
use plain HTTP inside a controlled network.

## SSH host key

An ed25519 host key is generated on first SFTP/SCP start and stored under
`ssh/host_ed25519_key` (`0600`). The Dashboard service detail shows its fingerprint. If a
device refuses to connect after you recreated the key, clear the old entry on
the client (`ssh-keygen -R "[host]:2222"` on macOS/Linux).

## Configuration

Persistent state lives in:

```
~/Library/Application Support/transferbuddy/
├── config.toml      # ports, binds, interface, host-key policy, upload settings
├── desktop.toml     # desktop root, window, theme, density, scale, panes
├── logs/            # transferbuddy.log (when file logging is on)
├── certificates/    # cert.pem, key.pem (self-signed, generated)
├── ssh/             # host_ed25519_key
└── state/           # secrets.toml (0600 — passwords, never in config.toml)
```

Settings changed in the TUI are saved automatically. CLI flags override the
stored configuration for the current run.

The shared root is deliberately **not** part of `config.toml`. The CLI serves
its working directory unless `--root` says otherwise; stale `root =` entries in
that file are ignored. The desktop remembers its separately chosen root in
`desktop.toml`. Shared service settings persist across frontends, while SSH
sessions, device credentials and jobs belong to each running process.

Beyond ports and services, `config.toml` also holds the UI preferences:

```toml
advertise     = "en12" # interface for generated URLs (absent = automatic)
auto_accept_host_keys = true # automatic device SSH key acceptance
speed_in_bits = true   # show speeds in bit/s instead of byte/s
sound         = true   # beeps when toggling settings (m in the TUI)
intro         = true   # animated intro screen on start
```

## TUI keys

| View | Keys |
|---|---|
| Global | `1`–`5` selects tab, `h` / `?` help, `m` sound, `q` quit |
| Dashboard | `Tab` services/transfers, `Space` enable, `s` start/stop, `Enter` edit |
| Connect | `a` add, `b` bulk/subnet, `c` CLI, `r` refresh/reconnect, `x` disconnect |
| Interactive CLI | keys forwarded to switch; `Esc` returns to the app |
| Transfer | `Tab` pane, `Enter` protocol/open/copy, `t` transfer, `p` protocol, `Del` / `D` remote delete, `Backspace` parent, `Home` root |
| Files | `R` refresh, `s` sort, `/` filter, `H` hashes |
| Upgrade | `Tab` files/jobs, `a` / `A` assign one/all, `d` copy+MD5, `V` verify existing, `Enter` device actions, `u` / `Y` install/YOLO, `i` cleanup |
| Logs | arrows scroll, `G` follow, `/` filter, `L` level, `P` protocol |

## CLI reference

```
transferbuddy [OPTIONS]

--root <DIR>            directory to share (default: current directory)
--http --https --ftp --sftp --scp --tftp --all
                        enable services (SFTP/SCP share one SSH service)
--port-http <P> --port-https <P> --port-ftp <P> --port-sftp <P> --port-tftp <P>
--bind <ADDR>           bind address for all services (default 0.0.0.0)
--interface <NAME|IP>   local interface whose address goes into generated URLs
--username <U>          FTP/SFTP/SCP user (default: cisco)
--password <P>          FTP/SFTP/SCP password (default: cisco123)
--uploads               allow uploads (off by default)
--upload-dir <DIR>      upload target, relative to root
--max-upload-mib <N>    upload size limit (0 = unlimited)
--no-tui                headless mode with structured logs on stdout
--no-intro              skip the animated intro screen
--no-sound              mute the TUI beeps
--log-level <L>         debug | info | warning | error
--log-file <FILE>       additional log file
--config <FILE>         alternative config file
--max-sessions <N>      parallel session limit
```

Exit codes: `0` clean shutdown · `1` no service could be started (e.g. port in
use) · `2` invalid configuration/arguments.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `port 80 needs root privileges` | run with `sudo` or use `--port-http 8080` |
| `port 8080 is already in use` | another server is running — pick a different port |
| Device can't reach the server | the URL carries one address and it may be the wrong interface — press `i` (or use `--interface`) to pin the one the device is on |
| `copy tftp:` times out | IOS talks to port 69 only — run `sudo transferbuddy --tftp` |
| `copy https:` fails on the device | the self-signed certificate isn't trusted; install it as a trustpoint or use HTTP |
| SCP/SFTP host key error on device | the host key changed; clear the old known-host entry on the device |
| Upload rejected | uploads are disabled by default (`--uploads`), files are never overwritten, size limit may apply |
| Transfer service fails to start | inspect Logs and Dashboard port/bind settings; `p` chooses another protocol |
| Deploy stops at "unexpected prompt" | the device asked something transferbuddy will not answer on its own; the transcript shows the question. Run that `copy` by hand |
| A session seems stuck | `x` disconnects in Connect; `r` reconnects with saved credentials. In interactive CLI, `Esc` returns and Ctrl-C is sent to the switch |
| Deploy fails with "host key … changed" | remove the named line from `state/known_hosts` if the device really was replaced |
| Flash space shows `—` | the session has not read `dir` yet, or the device answered something unexpected — press `r` in the upgrade view |
| Ping always shows `—` | transferbuddy shells out to the system `ping`; a firewall dropping ICMP looks the same as a device being down |
| Wrong directory shared | the root always follows the working directory — check the dashboard's `root:` line, and `--root` if you passed it. Older builds pinned the root in `config.toml`; the stale `root =` key is now ignored, so re-installing is enough |

## Development

```bash
cargo run -- --http --no-tui --root ./images     # run from source
cargo test --locked --workspace                  # both frontends and shared core
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
```

The Rust workspace keeps presentation separate from the shared engine:

```text
Cargo.toml                  # one shared major.minor.patch version
src/
├── main.rs                 # CLI/headless startup
├── cli.rs                  # arguments
└── tui/                    # ratatui views, input, console and workflow editor
crates/
├── core/src/
│   ├── engine/             # commands, queue, workflow, preflight and recovery
│   ├── services/           # HTTP/HTTPS, FTP, SFTP/SCP, TFTP adapters
│   ├── switch.rs           # SSH sessions, facts and device commands
│   ├── upgrade.rs          # verified INSTALL upgrade policy
│   └── …                   # config, filesystem, authentication, logs, parsers
├── desktop/src/            # egui views, native menu/tray and preferences
└── port-helper/src/        # optional macOS privileged listener broker
packaging/                  # bundle metadata, helper plist and app icons
scripts/                    # macOS packaging/release and icon generation
testdata/                   # recorded Cisco outputs used by parser tests
docs/                       # architecture and generated screenshot gallery
```

Both frontends use the same typed commands, snapshots, queue and SSH driver.
The CLI builds without desktop dependencies. Protocol adapters share the
service lifecycle and transfer metrics. See [architecture and checks](docs/desktop.md).

### Release builds

On macOS, install Xcode Command Line Tools, Python 3.11+ and both Rust targets:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo install cargo-packager --version 0.11.8 --locked
python3 scripts/release-macos.py
```

This creates one desktop DMG and one CLI `.tar.gz` per architecture, plus
`SHA256SUMS.txt`, under `dist/release-<version>`. It verifies binary versions,
architectures, app signatures and DMG checksums; it does not publish to GitHub.
Use `--target aarch64-apple-darwin` or `--target x86_64-apple-darwin` for one
architecture. The manual GitHub Actions workflow produces the same artifacts.

For cloud-synced checkouts, keep the build cache local:

```bash
CARGO_TARGET_DIR=/tmp/transferbuddy-build python3 scripts/release-macos.py
```

Without signing credentials, builds are ad-hoc signed. Optional
`TB_MAC_SIGNING_IDENTITY` and `TB_NOTARY_PROFILE` enable Developer ID signing
and DMG notarization. See [distribution details](docs/desktop.md#packaging-and-distribution).

## Guided workflows and connection recovery

Start **New workflow…** in Dashboard (GUI: Cmd/Ctrl+Shift+N; TUI: W) for guided
connect, file selection, preflight, transfer, install and results. Work continues
in the background, with shared core behavior in both frontends. Profiles preserve
source/destination mappings without SSH credentials; results export to CSV.

Previously connected SSH sessions recover automatically after network failure.
Pending copies stay paused until reviewed and explicitly resumed. Unknown copy
results can be inspected by size and MD5; partial destinations require an explicit
`y` overwrite confirmation. Copy URL pins never silently fall back to a different
interface. Authentication failures stop automatic login attempts; expected reboot
recovery has a three-request authentication limit. See [desktop details](docs/desktop.md).

## Global copy interface and appearance

The **Links** selector next to Settings stays visible in every desktop view. It
shows the effective Copy URL interface/IP for the selected device and protocol,
including fixed service binds. Automatic without a selected device remains
per-device; different protocol binds show Multiple IPs and their mapping in the
menu. Interface names follow DHCP changes; removed pins show Unavailable.
Changing the selector affects new commands and explicit retries, not running
copies. **Light/Dark** switches appearance in the header; Compact density is
configured in Settings and remains the default.

## License

MIT. Created by Samuel Heinrich.
