# TransferBuddy roadmap

## Windows desktop port — deferred, open for contributions

**Status:** proposal only. Implementation is deferred, with no target release,
scheduled start, or committed implementation owner. TransferBuddy does not
currently claim Windows support. This plan is available for a future maintainer
effort or a community contribution; documenting it does not start the port.

Contributors are welcome to open an
[issue](https://github.com/samuelheinrich/transferbuddy/issues) to coordinate a
proposed implementation and submit focused pull requests against the milestones
below. Please discuss substantial scope changes before starting them. The
acceptance criteria still apply to a contributed implementation.

Planning baseline: **0.2.14**, assessed on **2026-10-05**. The scope and estimates
below are planning assumptions, not a delivery commitment. Recheck the code,
dependency requirements, and external documentation before implementation.

### Goal and current state

The first Windows milestone would be an **internal desktop pilot for Windows 11
x64**, retaining all six transfer protocols and the existing Cisco workflows.
It would include an initially unsigned installer with optional firewall setup,
plus a ZIP distribution. Supported file roots would be local NTFS volumes,
including NTFS-formatted USB drives. A separately distributed Windows CLI/TUI
would follow as an additional milestone.

The existing architecture makes this feasible: the terminal and desktop apps
share the Rust core, while egui/eframe, wgpu, file dialogs, and tray libraries
already have Windows implementations. Replacing the GUI framework or duplicating
the protocol and Cisco business logic is not part of the plan.

The baseline is not yet Windows-buildable. Unconditionally imported Unix file
APIs block compilation; several macOS integrations would also be missing or
behave incorrectly on Windows. `cargo test --locked --workspace` passed on the
planning Mac, but no Windows build, installer, or runtime behavior has been
verified. Existing macOS tests are not evidence of Windows readiness.

### Development and builds from macOS

Use macOS for most development, common-core regression tests, and Windows cross
builds. Use Windows environments for native validation and packaging:

| Environment | Purpose |
| --- | --- |
| macOS | Main development, shared logic, regression tests, Windows cross builds |
| Windows ARM VM on Apple Silicon | Quick GUI, installer, and Windows API checks with the x64 app under emulation |
| Windows x64 VM over RDP | Native MSVC builds, automated Windows tests, installer and ZIP packaging |
| Windows 11 x64 hardware | Final checks for graphics, LAN, firewall, VPN, and power management |

A Windows VM on the development Mac is possible, a remote Windows VM is
available over RDP, and physical test devices may be available later. First
confirm the remote VM's architecture. If it is x64, use it as the native build
and test reference. Keep its repository checkout on a local NTFS volume rather
than a Mac shared folder when testing Windows filesystem behavior.

Windows 11 on ARM can emulate x64 applications. This makes an Apple Silicon VM
useful, but it does not replace acceptance testing on the supported x64 platform.
[Microsoft: emulation on ARM](https://learn.microsoft.com/en-ca/windows/arm/apps-on-arm-x86-emulation)

Retain Rust **1.94.0** initially. The Mac cross-build environment needs the
`x86_64-pc-windows-msvc` target, a full LLVM installation with `clang-cl` and
`lld-link`, `cargo-xwin` for SDK/CRT setup, and CMake, Ninja, and NASM for native
dependencies. An example setup, to be performed when implementation starts:

```bash
brew install llvm cmake ninja nasm
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin --locked

# Add Homebrew's LLVM bin directory to PATH as well.
cargo xwin build --locked --release \
  -p transferbuddy-desktop \
  --target x86_64-pc-windows-msvc
```

Use the project's pinned Rust compiler for that command. Pin the other build
tools to the versions validated by the initial build milestone.

`cargo-xwin` supports this approach and can generate CMake cross-build
configuration. The first technical milestone must compile **and link** the full
dependency tree. Non-Windows cross compilation is not an officially supported
Rust MSVC build path, so native Windows builds remain the release reference.
[cargo-xwin](https://github.com/rust-cross/cargo-xwin),
[Rust: Windows MSVC targets](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html)

The dependency tree already includes `aws-lc-sys` and `ring`; installing a Rust
target alone does not provide the native build prerequisites. Keep the existing
cryptography stack initially and supply the required compiler and assembler
tools. [AWS-LC: Windows requirements](https://aws.github.io/aws-lc-rs/requirements/windows)

On the native Windows VM, install Rust 1.94.0, Visual Studio Build Tools with C++
tools and the Windows SDK, plus the native dependency tools above. Generate
Windows packages there and continue building macOS packages separately.

### Implementation milestones

#### 1. Make the shared core buildable and safe on Windows

Extend the existing platform boundaries while keeping protocol and Cisco logic
shared. Add these OS implementations and minimum interface changes:

| Area | Planned change |
| --- | --- |
| SFTP file access | Wrap Unix `read_at`/`write_all_at` behind shared functions; implement Windows `seek_read`/`seek_write`, including partial-write handling |
| Available disk space | Add a Windows implementation of `free_disk_space` alongside Unix `statvfs` |
| Passwords and private keys | Use common private-file creation: Unix `0600`, Windows restricted DACLs for the user, SYSTEM, and administrators |
| Persistence | Store Windows state under `%LOCALAPPDATA%\transferbuddy`; preserve the TOML format and explicit configuration paths |
| Received files | Verify finalization, overwrite prevention, cancellation cleanup, and sharing errors on NTFS; release file handles cleanly before finalization and cleanup |
| Tests | Keep Unix permissions and symlink tests, and add equivalent Windows coverage |

Treat filesystem containment as a separate task. Extend root validation to cover
drive prefixes, UNC/device paths, backslashes, NTFS alternate data streams such
as `image.bin:stream`, reserved device names, and junctions/reparse points.
Reject invalid Windows path components on both reads and writes, and keep
resolved targets inside the shared root. Continue separating local filesystem
paths from Cisco and URL paths.
[Microsoft: Windows filenames and paths](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file)

#### 2. Port network behavior

- For unset Windows desktop ports, use TCP **21, 22, 80, 443** and UDP **69**;
  preserve saved choices.
- Continue binding listeners directly. Keep the macOS port helper confined to
  macOS and exclude it from Windows packages.
- Explain bind failures using Windows causes: occupied port, invalid address,
  or access restriction. Replace generic `sudo` advice on Windows. Winsock
  distinguishes these failure cases.
  [Microsoft: `bind`](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-bind)
- Replace the Unix `ping` command on Windows with the native ICMP API, using the
  corresponding IPv6 API when needed. Avoid parsing localized console output.
  A missing ICMP reply must not override a successful SSH connection.
  [Microsoft: `IcmpSendEcho`](https://learn.microsoft.com/en-us/windows/win32/api/icmpapi/nf-icmpapi-icmpsendecho)
- Classify Ethernet, Wi-Fi, VPN, and virtual adapters using Windows metadata.
  Add a stable adapter identifier while continuing to accept existing saved
  names and IP addresses and displaying friendly names.

#### 3. Complete desktop integration

- **Single instance:** retain the existing file lock; add a Windows named pipe
  scoped to user and configuration. A second launch requests that the existing
  window be shown.
- **Tray and window lifecycle:** closing hides the app; the tray menu and a
  double-click reopen it. If tray creation fails, do not let the window become
  unreachable.
- **Keyboard and menus:** use Ctrl-based Windows commands, keep the existing
  egui menu bar, and adapt the tray commands currently based on `META`.
- **Sleep inhibition:** hold a Windows power request while transfers or upgrade
  jobs run, and release it afterwards.
- **Sound:** replace macOS `afplay` with asynchronous Windows system sounds.
- **Startup:** launch release builds without an extra console window; expose
  startup failures through a dialog and log. Embed an icon, version resources,
  and DPI/long-path manifests.
- **Graphics:** retain wgpu with DX12 and Vulkan, and use the system FXC shader
  compiler for the pilot. Test RDP and virtual GPUs explicitly; show an
  actionable startup error if no suitable adapter is available.

#### 4. Add the installer, firewall setup, and CI

Extend packaging with **cargo-packager and a custom NSIS template**.
[Cargo Packager configuration](https://docs.crabnebula.dev/packager/configuration/)

The installer and distribution behavior would be:

- Install into `Program Files` with elevated installation permissions; run the
  application as a normal user afterwards.
- Offer an optional firewall setup checkbox, unchecked by default.
- Create two inbound rules bound to the full executable path, for TCP and UDP,
  on **Private/Domain** profiles, initially restricted to **LocalSubnet**.
  Leave Public closed.
- Program rules must cover dynamic FTP data ports and TFTP transfer sockets;
  opening only control ports 21 and 69 is insufficient. Routed device networks
  require additional IT-managed rules.
- Create/update rules idempotently. On uninstall, remove only TransferBuddy's
  rules and retain user data by default.
- Also publish a ZIP without an installer or automatic firewall changes.
- Do not launch the application with the installer's elevated identity. Require
  running jobs to end normally before updating application binaries.
- Link the release CRT statically and inspect runtime dependencies on a clean
  Windows machine; include any additional runtime files actually required.

Firewall setup cannot override explicit block rules or central enterprise
policy. [Microsoft: firewall rules](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/rules)

Add Windows CI for the initial core/desktop scope:

```bash
cargo test --locked -p transferbuddy-core -p transferbuddy-desktop
cargo clippy --locked -p transferbuddy-core -p transferbuddy-desktop \
  --all-targets -- -D warnings
```

Make shared protocol tests runnable directly against the core. Retain the
current CLI-process Unix-SIGINT check in the Unix test path. Keep full workspace
checks on macOS and the existing Linux CLI checks.

### Acceptance tests

| Test group | Acceptance criteria |
| --- | --- |
| Protocols | All six protocols with real local clients; active/passive FTP, TFTP dynamic ports, upload cancellation, and overwrite prevention |
| Files | Unicode, spaces, long paths, different drives, alternate data streams, junction escapes, locked files, and private-file permissions |
| Desktop | All five views, clipboard, Ctrl shortcuts, folder picker, tray, second launch, normal quit, and active jobs |
| Display | Windows scaling at 100/150/200%, monitor changes, and RDP connect/disconnect |
| Installation | Clean install, upgrade, and uninstall; with/without firewall setup; subsequent app operation without administrator rights |
| Network | Access from a second machine with the firewall active, multiple adapters, and VPN; loopback tests alone are insufficient |
| Cisco | Firmware transfer with hash comparison, reverse transfer, SSH console, and INSTALL with reload/reconnection on a dedicated test device |
| Regression | Existing macOS and Linux CLI checks remain successful |

The pilot does not add support for new Cisco device families. Validate existing
workflows on the new host platform. Native x64 hardware checks are required
before describing the pilot as validated for Windows 11 x64.

### Known limitations and deferred extensions

- **macOS integrations:** LaunchDaemon registration, audit-token and Developer
  ID checks, and macOS Settings links have no direct Windows migration. Supply
  Windows-specific behavior rather than carrying those integrations over.
- **Sleep protection:** cannot prevent forced sleep or power loss. Windows
  Modern Standby can limit power requests on battery power.
  [Microsoft: power requests](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-powersetrequest)
- **Unsigned pilot:** SmartScreen or enterprise policy can warn or prevent
  launch. Later signing establishes publisher identity but does not guarantee
  immediate SmartScreen reputation.
  [Microsoft: SmartScreen](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
- **Filesystem scope:** exFAT, SMB, and cloud folders are not approved for the
  pilot. The current hardlink-based overwrite protection needs an alternative
  for exFAT.
- **Filenames:** some macOS-valid names cannot be represented on Windows.
  Reject unsupported names with a clear error.
- **Virtual machines:** ARM emulation and virtual graphics do not replace x64
  hardware tests. VM NAT can prevent inbound Cisco transfers.
- **Process lifetime:** tray operation ends at logout or process exit. A Windows
  service, automatic restart, and an auto-updater are outside the pilot.
- **Platforms:** native Windows ARM64, Windows 10, and Windows Server have no
  committed support in this proposal.

### Effort estimate and assumptions

| Work package | Person-days |
| --- | ---: |
| Build environment, Windows CI, and first complete build | 2–3 |
| Core port, filesystem safety, and NTFS behavior | 4–6 |
| Network and desktop integration | 3–4 |
| Installer, firewall setup, and ZIP | 2–4 |
| Windows acceptance, Cisco tests, fixes, and documentation | 4–6 |
| **Base effort** | **15–23** |
| **Planning budget including contingency** | **20–30** |

At eight hours per person-day, budget **160–240 hours**, or approximately
**4–6 weeks of focused work by one person**. Waiting for hardware or IT approval
adds calendar time. After the first 2–3 days, the build milestone should provide
a firmer assessment of dependency risks.

Separately estimated follow-up work:

- **Windows CLI/TUI:** another **3–5 person-days** for clipboard, terminal
  behavior, process signals, and separate packaging.
- **Public signed release:** another **2–4 person-days**, plus procurement and
  validation of a signing identity.
- **Native Windows ARM64:** another **3–5 person-days**, plus its own hardware
  acceptance tests.

These estimates assume Rust experience, basic Windows API knowledge, access to
the remote Windows VM, and a Cisco lab device. VM/Windows licenses, hardware,
and later signing are separate costs. There is no budget or delivery date
committed by this roadmap.

Follow `AGENTS.md` for each completed implementation change set: increment the
workspace patch version and update `Cargo.lock`; show the full major.minor.patch
version in the GUI, CLI/TUI, and package metadata.
