# Shared desktop/TUI architecture — 0.2.14

The [0.2.14 release](https://github.com/samuelheinrich/transferbuddy/releases/tag/v0.2.14)
provides separate macOS 13+ Intel and Apple Silicon desktop DMGs and CLI archives.
See the [installation guide](../README.md#installation) and
[current screenshot gallery](screenshots/README.md).

The workspace version in the root `Cargo.toml` is the source for every binary,
crate, title, `--version` output and installer. The desktop uses egui/eframe
0.35, compatible with the repository's Rust 1.94 toolchain. Core has no UI
framework dependency. `cargo build` and `cargo test` default to the existing
terminal package; use `--workspace` to include desktop and port-helper.

| Package | Responsibility |
| --- | --- |
| `transferbuddy-core` | SecureRoot, protocols, Cisco policy, SSH sessions, shared terminal parser, device facts, jobs, configuration and logs |
| `transferbuddy` | CLI parsing, headless output, ratatui presentation and keyboard/mouse input |
| `transferbuddy-desktop` | egui presentation, native folder dialogs, menu/tray, window preferences and single-instance handling |
| `transferbuddy-port-helper` | macOS root listener broker, authenticated local IPC, socket descriptor handoff |

Both adapters send typed `engine::Command` values. `submit` returns an operation
ID; `snapshot`, `watch` and `subscribe` expose state and events. Worker tasks
start listeners, queue transfers and run switch commands without a rendering
loop. The kernel does file listing and streaming hashes on blocking workers;
network tasks stay on Tokio. Submission is serialized so two rapid actions
cannot reserve the same device. Separate processes have separate kernels and
SSH sessions. A port collision becomes a service failure; an app never shuts
down another process's services.

The engine owns image assignments, operations, transfer permits and pending
confirmation IDs. Approval is exactly `y`, expires after use and rechecks the
device state and verified image. INSTALL confirmation binds the image path,
MD5 and target version; a stale question cannot approve a changed image.
The device driver also rechecks remote MD5, current versions and boot state
before executing INSTALL. Platform mismatch warns and allows copying.

The vt100 parser belongs to the SSH session. TUI and GUI render its grid;
raw input and PTY resizing use the same core API. Automation transcripts remain
separate. Closing CLI closes its extra channel and returns to the adapter.
The desktop supports one CLI window per device and several devices at once.
Minimize, Escape and background clicks hide these windows without ending their
CLI channels; taskbar tabs restore them. TUI Escape closes its CLI channel.
Ctrl+C interrupts the device; Cmd/Ctrl+C with selected text copies locally.
Paste sends only to the focused terminal. Black console visuals are independent
of the app theme. Terminal layout jobs are cached by output revision and grid
size, while egui's font cache handles DPI and scale changes.

## Persistence and lifetime

`config.toml` and server `state/secrets.toml` retain the existing shared format.
Configuration writes use an atomic replacement. `desktop.toml` contains only
root, window size, theme, density, UI scale, reduced motion, pane sizes and column
widths. New installations use Compact density and 0.85 interface scale; the
Settings slider adjusts scale in 0.05 steps. Older files using the previous
standard defaults migrate once to this density and scale. Other saved choices
are preserved. Older preference files receive defaults for new fields. Invalid or
nonfinite layout values are discarded. Device passwords are not persisted.
The desktop takes a file lock per configuration directory. A same-user Unix
socket receives the fixed `show` signal and wakes the existing window. A stale
socket is replaced only by the lock holder. TUI does not take this lock.

Window close cancels the native close and hides the viewport. Menu/tray events
reopen it. Core work continues; confirmations remain pending and the tray
indicates that attention is needed. Explicit quit asks if operations are
active, then stops services and closes SSH sessions. If native tray creation
fails, closing falls back to the normal quit flow to avoid an inaccessible app.
A macOS NSProcessInfo activity token is held while jobs or file transfers run;
this prevents idle sleep, not a user-forced sleep or power failure.

## Standard-port helper

Services call `platform::bind_tcp` or `bind_udp`. Normal binding is tried first.
Only `PermissionDenied` invokes the optional broker; address conflicts are
reported directly. The helper supports TCP 21, 22, 80, 443 and UDP 69, with
literal IPv4/IPv6 bind addresses. Other privileged ports require a normal
privileged launch or a different port choice.

The signed bundle embeds `Contents/MacOS/transferbuddy-port-helper` and
`Contents/Library/LaunchDaemons/com.transferbuddy.port-helper.plist`.
SMAppService registers/unregisters the daemon. The GUI never runs as root.
The helper obtains LOCAL_PEERTOKEN from the Unix socket, validates the calling
code through Security.framework, and requires `com.transferbuddy.desktop` with
the same Developer ID Team ID as the helper. Unsigned/ad-hoc clients fail
closed. The client checks the helper peer UID is root. Requests are bounded,
time-limited and restricted to listener creation; SCM_RIGHTS hands the socket
back to Tokio. No filenames, shell commands or credentials cross this IPC.

## Feature parity and testing

The five numbered workspaces are defined in `core::workspace` and displayed in
the same order by both frontends. Desktop presentation is split into shared
design components, shell/command palette, workspace views and console windows
with a bottom taskbar. Dashboard status counts are plain header text, service
actions are adjacent to their service, and interface selection sits beside them.
Interface discovery runs off the frame thread and refreshes on request.
SSH login, enable and desktop service password fields are masked. Copy URLs
retain credentials. Header status counts share the logo's row; connection
search sits on the right and Remove disconnects/removes idle devices. Busy
device jobs must finish or be cancelled first. TUI Connect supports Delete.
Upgrade rows show the normalized current and target releases with orange
for a change and red for a downgrade. Logs show hostname, IP, model and protocol;
copy commands highlight the advertised server address.
JetBrains Mono NL 2.304 (SIL Open Font License) is bundled locally; there is no
font download at runtime. `scripts/generate-icons.py` reproduces the packaged
console icon using only the Python standard library.

The desktop uses normal Tab focus between controls. In file tables, Enter or a
double-click opens folders or file details; copying is explicit through the
source/destination action bar or Cmd/Ctrl+Enter. Cmd/Ctrl-click toggles files and
Shift-click selects a visible range. Dragging between browsers uses the same
queue; remote downloads use FTP. At small window sizes or high zoom, Local/Remote and Images/Device jobs
switchers preserve usable lists; larger windows keep both sides visible.
Each device remembers its browser folders,
filters and selections for this session.

Cmd/Ctrl+1–5 changes workspaces, Cmd/Ctrl+K opens the searchable command palette,
and Cmd/Ctrl+J opens the common Jobs drawer. On short windows the Jobs view uses
the workspace area so job actions stay reachable. macOS menus and the
Windows/Linux menu bar share the same command catalog and availability checks. Escape closes
dialogs and minimizes the focused CLI. Terminal input requires terminal
focus; toolbar navigation stays in the GUI. GUI deletion, cleanup and install
approvals require manually typing lowercase `y`, then clicking Confirm. Pasting
`y`, uppercase `Y`, and the key that opened the question cannot approve it.

Transfers freeze device, root, local/remote paths, direction and protocol.
Jobs run FIFO per device and in parallel across devices. Disconnected queues
pause after disconnection or a laptop interface/route change. Review pending
jobs, then explicitly Resume checked; Retry keeps the original source
fingerprint and destination. Copy URLs are rebuilt from current interface and
service settings at execution, including retries of upload/verify image jobs.
Retry with interface applies the selected address successfully before queuing
the retry. The original failed operation keeps its previous copy command;
the new operation records the actual command it sends. A changed source must be selected again. Queues
and SSH credentials are session-only and are never saved in preferences.
Cancellation removes pending work or attempts Ctrl+C on an active copy, cancels
only its matching local data stream, and waits up to ten seconds for the device
prompt. Without that prompt the job reports Remote status unknown and requires
inspection of possible partial files. Remote partial files are never removed
automatically. Received files are written through temporary files and finalized
without silently replacing an existing destination.

GUI submissions, dropped-path validation, directory listing and hashes run off
the frame thread. The engine publishes changed state; idle or paused queues do
not request periodic repaints or prevent system sleep. File/log lists are
virtualized and filtered results are cached. Preferences preserve existing
files and remember layout, appearance and drawer size.

Each frontend declares its supported action catalog and tests it against core.
The catalog is a release reminder; behavioral tests and the manual checks below
verify actual parity. Add a new action to core and both adapters in one change.
Shared policy must stay in core, including validation and confirmation rules.

| Area | TUI | Desktop | Shared policy |
| --- | --- | --- | --- |
| Services/settings | Dashboard keys/popups | Dashboard controls/settings dialog | ServiceManager, Config, listener binding |
| Connect/discovery/reconnect | Add/bulk/CLI shortcuts | Forms, device table and CLI window | Engine, SwitchManager |
| Local/remote files and protocols | Focused panes and keyboard | Resizable panes, mouse and keyboard | SecureRoot, file model, protocol priority |
| Transfer/receive/delete | Enter copies; red confirmation | Explicit Copy action; red confirmation | Engine permits, exact confirmation, Cisco commands |
| Hashes | Hash popup | Hash dialog and clipboard | Streaming digest worker |
| Image assignment/verify/install | Split view and row actions | Split view and row buttons | Engine assignments and Upgrade state |
| Reboot/retry/cleanup | Session view and action menu | Progress row and console | Shared switch driver |
| Logs | Search/filter/follow | Search/filter/follow/copy | Logger and event stream |
| Guided workflows | W assistant, keyboard steps | New workflow, Cmd/Ctrl+Shift+N | Core workflow/preflight/job ownership |
| Interrupted work | J jobs: v inspect, o overwrite, s review, S resume | Jobs drawer and recovery actions | Size + MD5, original source stamp, exact y |
| Profiles/reports | Assistant result: P/s, E/e | Assistant result controls | Non-secret TOML profiles, CSV under root |

Automated checks:

```bash
cargo fmt --all --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test -p transferbuddy-desktop desktop_preview -- --ignored
cargo test -p transferbuddy-desktop large_workspace_profile -- --ignored --nocapture
```

The preview command renders all five views at 900×600, 1280×800 and 1440×900
in dark/light and standard/compact layouts under `dist/gui-review`, including
the 0.85 MacBook default, multiple CLI windows at 150% zoom, and connection
dialogs. It requires a GPU adapter. Review images use detached
fixture devices, not live equipment. The profile command measures CPU layout
time with 100 devices, 10,000 files and 5,000 logs; it does not measure GPU FPS.
Normal UI tests use egui_kittest with detached devices;
shared driver tests use real local SSH/FTP/HTTP/TFTP fixture servers.

Before releasing, exercise both adapters against the same disposable switch:

1. Start/stop each protocol, transfer in both directions, compare hashes.
2. Navigate nested directories and ensure the chosen root cannot be escaped.
3. Open multiple CLI windows, select/copy/paste text, interrupt with Ctrl+C,
   drag/resize, minimize with Escape and background clicks, restore via taskbar,
   and close or exit each channel. Ensure input reaches only the focused device.
4. Connect mixed IP/subnet lists; check failures in Logs and reconnect after VPN loss.
5. Assign one/all images, warn on platform mismatch, verify an existing image;
   confirm same-version, missing-image and hash-mismatch upgrades are blocked.
6. Decline and accept deletion, cleanup and normal/YOLO install confirmations;
   check actual reload approval and post-reboot version/stack verification.
7. Hide/reopen the GUI during a transfer and reboot; pending prompts must wait.
8. Test second launch, explicit quit, changed root, GUI/TUI port collisions,
   helper approval, helper removal and standard TCP/UDP ports.
9. On macOS 13+ Intel and Apple Silicon, launch the installed `.app`, verify
   Developer ID signatures and Gatekeeper/notarization, then repeat port checks.

The signed helper and real-device upgrade/reboot checks require external
credentials/hardware; unit tests do not establish those deployment results.

## Guided workflows and recovery (0.2.12)

Dashboard's **New workflow…** opens an optional assistant. It uses the existing
five views and connection manager. Choose transfer or firmware upgrade, select
existing or new devices, choose files (one shared release or per-device images),
then check the protocol, Copy URL interface, sources, readiness and space.
Transfers use the shared per-device queue; the assistant can be closed and
reopened while work continues. Normal transfers support arbitrary files and
FTP downloads into separate device folders. Upload/verify never starts an
installer. Each install needs its own explicit confirmation and retains the
verified-image/different-version gates.

The shared supervisor reconnects previously established SSH sessions after
unexpected network failure, with 2/5/10/20/30-second retry delays and retained
in-memory credentials. It never replays queued work or installs. Intentional
disconnects stop the supervisor. Authentication or algorithm rejection stops
automatic login attempts; edit login details and reconnect manually. An expected
upgrade reboot checks TCP reachability independently of ICMP. Across password
and keyboard-interactive methods it permits at most three actual authentication
requests, using 30/60-second delays after rejected login rounds. Service-start
and transport failures may be retried while that budget remains.

Laptop interfaces and source routes are refreshed in a background worker. An
interface name follows its current address after DHCP; a literal address stays
pinned and becomes unavailable if removed. Explicit service binds take priority
and remain visible. An unavailable selection never silently falls back to a
different interface. A route/interface change pauses pending jobs; queue review
checks readiness, the original source fingerprint and the current Copy URL.
Changing endpoint settings invalidates previous reviews. Active copies may
finish or fail normally; if the remote result is unknown, inspect before retry.

Destination inspection reads directory size and compares remote/local MD5.
A matching destination completes the interrupted job without copying; a missing
file can be retried; a different/partial file needs **Overwrite & retry** and
exactly lowercase `y`. Inspections and retries do not repeat an install. Received
files retain the existing temporary-file and no-overwrite policy.

Result controls save non-secret work profiles (hosts/ports, source/destination
mappings, protocol and Copy URL selection) to `profiles.toml` in the config
folder. SSH credentials are never included. Profiles reconnect missing hosts
through the normal credential form. CSV reports contain device identity,
old/current release, verified upgrade MD5, status, error and elapsed workflow
time. Reports are created under the selected root and do not overwrite existing
files. Stop workflow services only stops listeners the workflow started and
refuses while other jobs or sessions use them.

Recovery fixture tests cover route changes, source mutation, stale queue reviews,
unknown/partial destinations, duplicate retries, exact one-use confirmation,
profile mappings, blocked prerequisites and bounded reboot authentication. Real
VPN/DHCP changes, RADIUS and device firmware still need the disposable-device
checks described above. Switch IP migration is outside this release.

## Packaging and distribution

`packaging/packager.json` is a template. `scripts/package-macos.py` injects the
workspace version and binary directory, builds all binaries with a macOS 13.0
deployment target, creates the `.app` through
[cargo-packager](https://docs.crabnebula.dev/packager/), validates bundle metadata,
and creates a DMG with an Applications shortcut. All output stays under `dist`.
`scripts/release-macos.py` wraps packaging for both architectures, adds standalone
CLI archives and writes `SHA256SUMS.txt`. It checks all three packaged binaries'
architecture, CLI/GUI versions, app signature and DMG integrity. The manual
GitHub Actions release workflow builds the same artifacts without publishing.
`CARGO_TARGET_DIR` is supported by both scripts; keeping it outside a cloud-synced
checkout avoids cloud-provider reads of build caches.

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo install cargo-packager --version 0.11.8 --locked
python3 scripts/release-macos.py
# dist/release-0.2.14/{desktop DMGs, CLI archives, SHA256SUMS.txt}
```

Public 0.2.14 artifacts are ad-hoc signed, without Developer ID notarization.
The authenticated standard-port helper therefore remains unavailable in these
builds; normal listener binding and high-port alternatives work independently.
When macOS blocks a first launch, use Privacy & Security → Open Anyway after
checking its source/checksum. Local Network permission is separate and still
required on macOS 15+.

Use `--out-dir dist/macos-native-<version>` when creating an update while an
older development app is running. This leaves its bundle untouched. Local
bundles receive an ad-hoc signature that binds Info.plist and resources with
the application identifier. This does not replace Developer ID signing:
[Apple TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
recommends an Apple-issued identity for reliable Local Network permission
tracking across builds. macOS still requires the user to approve local network
access for the app. The app cannot grant itself that permission.

In Connect, select a failed or offline device and use **Test SSH transport**.
This runs the same negotiation inside the GUI process, without attempting
login or saving a host key. A successful Terminal connection does not establish
that macOS has allowed the GUI process to access the local network. The failed
device details and Help include a shortcut to Local Network settings.

An optional command-line transport diagnostic can reproduce a real device's
handshake without attempting authentication or saving a host key:

```bash
TB_SSH_DIAGNOSTIC_HOST=192.0.2.5 cargo test -p transferbuddy-core ssh_handshake_diagnostic -- --ignored --nocapture
```

For local Developer ID signing, export `TB_MAC_SIGNING_IDENTITY`. The script
signs the helper, packaged CLI and app in order and verifies the complete bundle.
For notarization, also set `TB_NOTARY_PROFILE` to a profile previously stored
with `xcrun notarytool store-credentials`. The script submits the DMG, waits,
staples and validates it. Signing and notarization credentials are not stored
in the repository. The locally signed development bundle is suitable for local use; its
privileged helper requires a Developer ID signature before it accepts requests.

Windows/Linux desktop packaging is not declared ready. Platform listener
interfaces and the independent Rust UI allow those ports without duplicating
business rules. OS-specific menu, IPC, CLI clipboard/ping and packaging behavior
still needs implementation and verification there.

## Header tools (0.2.13)

**Links** beside Settings displays the current copy address from cached network
facts. Its menu offers Automatic, interfaces, manual interface/IP input and IP
copy actions, with a protocol-by-protocol view of fixed bind overrides. The
selector stays in the separate toolbar when the window is narrow. Active copies
keep their command; retries rebuild from the latest setting. Settings, Help and
Info open aligned to the right below the toolbar; Settings and Info have larger
initial sizes. Device CLI, Actions and Remove share one row. Light/Dark replaces
the density toggle; Compact remains the default and is adjustable in Settings.
Device host keys are accepted automatically by default for individual, bulk and
subnet connections. The Settings switch is saved and applies to subsequent
connections and reconnects; disabling it restores unknown-key questions and
changed-key rejection.

At narrow widths, secondary header tools move to **More**; Links, Settings and
Light/Dark stay directly available. Connect keeps Add/Bulk and search on one
row, with attention filtering, cleanup and device details under **Filters**.
Selection actions use a compact menu to preserve a visible, clickable device list.
