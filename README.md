# transferbuddy

A native macOS CLI/TUI file transfer server for provisioning **Cisco** switches,
routers, wireless controllers and similar network devices. Point it at a
directory and it serves the files over **FTP, HTTP, HTTPS, SCP, SFTP and TFTP**
simultaneously — with a keyboard-driven terminal UI showing live sessions,
transfer speeds and ready-to-paste `copy` commands.

![transferbuddy dashboard](docs/screenshots/dashboard.svg)

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
- **Deploy from the file browser:** `d` opens an SSH session to the switch and
  runs the `copy` for you — and nothing else. transferbuddy can type six kinds
  of line on a device and a configuration command is not one of them.
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
  generated on first use; no dependencies, no telemetry, no outbound
  connections.

## Installation

### Homebrew (planned)

```bash
brew install transferbuddy
```

Until the formula is published, install from a local tap using
`Formula/transferbuddy.rb` in this repository:

```bash
brew install --build-from-source Formula/transferbuddy.rb
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

```bash
cargo install --path .
# or:
cargo build --release        # binary in target/release/transferbuddy
```

Native builds work on both Apple Silicon (arm64) and Intel (x86_64) Macs; see
[Release builds](#release-builds) for cross-building and universal binaries.

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
select which services are *enabled*. Start them in the services view with `s`
(selected service) or `S` (all enabled ones). Headless mode (`--no-tui`) has no
such control and starts the enabled services immediately.

Press `h` or `?` inside the TUI for the key reference. In the file browser,
press `Enter` on a file to see the Cisco copy commands and `y` to copy one to
the clipboard.

## The TUI

A keyboard-driven Atari-8bit-flavoured terminal UI: dark CRT background, amber
titles, phosphor-green highlights. Five views, switched with `1`–`5`, `Tab` or
`←`/`→`; the current view's keys are always listed in the footer.

### Intro

Every start opens with a short block-letter animation: the logo wipes in, the
classic rainbow bars scroll through it, the subtitle types itself out and a
loading bar fills up. Any key fast-forwards to the end.

![transferbuddy intro animation](docs/screenshots/intro-anim.svg)

The screen then *stays* until you press `Enter`, `Space` or `Esc` — nothing
starts behind your back, and you get to look at it as long as you like. Skip
the whole thing with `--no-intro` (or `intro = false` in `config.toml`).

![transferbuddy intro, ready to start](docs/screenshots/intro-ready.svg)

### 1 · Dashboard

Everything at a glance: shared root, the local address to type into the switch,
the credentials, transfer totals and the log file location. Below it the
service table with status, port, bind address, active sessions, uptime and a
clear **CLEARTEXT** / encrypted marker. The bottom half has quick panels for
files, sessions and local interfaces, plus a full-width live log block.

![transferbuddy dashboard](docs/screenshots/dashboard.svg)

### 2 · Services

Start, stop and configure each protocol. `Space` enables/disables a service,
`s` starts or stops it, `S` starts every enabled one, `X` stops everything. The
detail pane below shows the credentials, the upload settings and — depending on
the selected service — the TLS certificate fingerprint or the SSH host key.

![transferbuddy services view](docs/screenshots/services.svg)

`Enter` opens the edit popup for the selected service, where port, bind
address, user, password and the upload directory can be changed in place.
Changes are saved immediately and running services are restarted for you.

![transferbuddy service edit popup](docs/screenshots/service-edit.svg)

### 3 · Files

Browse the shared directory: sizes, timestamps, file type and the exact
download path each device would use. `s` cycles the sort order, `/` filters,
`Backspace` goes up, `H` computes MD5/SHA-256/SHA-512 and can compare a pasted
hash against them.

![transferbuddy file browser](docs/screenshots/files.svg)

`Enter` (or `y`) on a file shows ready-to-paste Cisco `copy` commands for every
enabled protocol — with the detected local IP, the configured port and the
credentials already filled in. `y` copies the selected line to the clipboard.

![transferbuddy Cisco copy commands](docs/screenshots/cisco-copy.svg)

`d` goes one step further and does it for you — see [Deploy](#deploy).

#### Deploy

Pressing `d` on a file opens an SSH session to a switch and starts the transfer
from the device side — no console, no copy-paste:

```
 switch IP / host  10.20.30.40
 ssh port          22
 username          netadmin
 password          •••••••
 enable password   ••••••
 protocol          http  (running)
 destination       flash:
 overwrite         no

 the switch will run  copy http://10.20.30.9:8080/cat9k_iosxe.17.09.bin flash:
```

`↑↓` moves between fields, `←→`/`Space` cycles the protocol and toggles
`overwrite`, `Enter` starts. The line above the keys is the exact command that
will be typed — the source address is the local IP that actually routes to the
switch, so a multi-homed machine advertises the right one.

From there the popup turns into the live session view: what transferbuddy sent
(`>`), what the device answered, and the `!!!!` progress marks of the running
`copy`, above a header with the device's facts. `c` cancels, `↑↓` scrolls, `r`
re-reads the facts, `x` disconnects. The transfer also shows up in the sessions
view like any other download, because that is what it is.

The session does **not** end with the copy. It stays open, appears in the
[switches view](#6--switches) and is ready for the next file — the second
deploy to the same switch needs no password at all.

**What transferbuddy is allowed to do on your switch**

The whole point of the feature is that it stays inside a very small box. Every
line that goes to a device passes a whitelist, and the whitelist has six
entries:

| Line | Why |
|------|-----|
| `terminal length 0` | otherwise the output stops at `--More--` |
| `enable` | `copy` needs privileged EXEC — skipped when the login is already at `#` |
| `show version` | IOS version, model, serial, uptime, stack members |
| `dir <device>:` | free flash space, read before and after every copy |
| `copy <transferbuddy URL> <device>:` | the actual transfer |
| `exit` | leave the session cleanly |

The two read-only ones are just as narrow as the rest: only `show version`, no
other `show`, and `dir` only on a storage device. Everything else is refused
before a byte is sent: `configure terminal`, `write`, `reload`, `delete`,
`erase`, `format`, `show running-config`, a `copy` into `running-config` or
`startup-config`, a second command chained with `;` or `|`. The copy
destination must be a storage device (`flash:`, `bootflash:`, `usbflash0:`,
`disk0:`, …).

The prompts `copy` asks are answered just as narrowly. `Destination filename`,
`Source filename` and `Address or name of remote host` are confirmed with
Enter; `Do you want to over write?` follows the `overwrite` setting; anything
about erasing or formatting is always declined; and a prompt transferbuddy does
not recognise ends the session instead of being confirmed blindly. The only
answers it can send are Enter and `n`.

Passwords are typed only at a `Password:` prompt, are shown as `••••` in the
form, as `********` in the transcript and never reach the log or `config.toml`.
They are cleared from the form the moment the session has them — host, user,
protocol and destination are kept, so the next file takes two keystrokes.

On the first connection the device's host key is shown as a SHA-256
fingerprint and, once accepted with `y`, stored in
`~/Library/Application Support/transferbuddy/state/known_hosts`. If a stored
key later no longer matches, the deploy fails and says which line to remove —
it is never silently accepted.

### 4 · Sessions

Every connection with protocol, source, user, file, direction, state, progress
and current speed. `B` toggles between bit/s and byte/s; the detail pane adds
average speed, duration and ETA.

![transferbuddy sessions view](docs/screenshots/sessions.svg)

### 5 · Logs

Structured, colour-coded log with live tail (`G`), text filter (`/`), minimum
level (`L`) and protocol filter (`P`). From the services view, `L` jumps
straight to the log filtered to that protocol.

![transferbuddy log view](docs/screenshots/logs.svg)

### 6 · Switches

Every SSH session transferbuddy holds open to a device, one per row: name (the
device's own hostname, taken from its prompt), address, user, state, IOS-XE
version, model, free flash and the current ping. The pane below shows the
selected device in full, including the stack members.

```
name             host              user      state   IOS-XE    model          flash free      ping
SG-AS-OG5-01     10.20.30.40:22    netadmin  ready   17.15.03  C9200L-48P-4X  235 MB (88%)    1.2 ms

 state    ready — 504057659 bytes copied in 728.176 secs (692220 bytes/sec)
 device   C9200L-48P-4X   IOS-XE 17.15.03   uptime 47 weeks, 6 days, 23 hours
 flash    235 MB free of 1957 MB (88% used) on flash:   ping 1.2 ms
 stack    1:17.15.03  *2:17.15.03  3:17.15.03
```

`Enter` opens the session with its full transcript, `r` re-reads `dir` and
`show version`, `x` disconnects, `X` drops closed rows from the list.

**Free flash space** comes from the footer `dir` prints —
`1956839424 bytes total (234979328 bytes free)` — and is shown in MB, with how
much of the device is used. It is read when a session opens and again after
every copy, so the number on screen is the one that matters for the next
image. When a session is open, the deploy form compares the file against it
and says outright whether the image still fits.

**The stack row** lists every member with its software version, the active one
marked `*`. Members that do not match the system version are highlighted —
that is the case where an upgrade only took on part of a stack.

**The ping monitor** checks every device every five seconds and shows the
round-trip time, or how long the device has been unreachable. That is what
makes a reload visible from here, and it is the mechanism the install
tracking will use to catch a switch the moment it comes back.

### Help

`h` or `?` opens the key reference — two tables, one key per line, keys
highlighted. On short terminals it scrolls with `↑`/`↓`.

![transferbuddy help](docs/screenshots/help.svg)

### Sound

Toggling settings gives short "PC speaker" style blips — one tone for on, a
different one for off, a third for a rejected value. `m` mutes them (the
indicator top right switches from `♪` to `×`), as does `--no-sound`. The
setting is remembered.

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

Requesting a privileged port without root produces a clear error and a
suggested alternative — nothing starts silently on a different port.

> **TFTP note:** Cisco IOS cannot specify a TFTP port; `copy tftp://…` always
> uses 69. To serve classic TFTP to IOS devices, run with `sudo` (or use HTTP,
> which every modern IOS-XE supports and which is faster).

Ports and bind addresses can be changed per service in the TUI (`p` / `b`) or
via CLI flags (`--port-http`, `--port-https`, `--port-ftp`, `--port-sftp`,
`--port-tftp`, `--bind`).

## Protocols

| Protocol | Encrypted | Auth               | Uploads | Cisco example |
|----------|-----------|--------------------|---------|---------------|
| HTTP     | no        | none (download)    | opt-in  | `copy http://192.168.1.10:8080/img.bin flash:` |
| HTTPS    | yes (TLS) | none (download)    | opt-in  | `copy https://192.168.1.10:8443/img.bin flash:` |
| FTP      | no        | user/password      | opt-in  | `copy ftp://user:pass@192.168.1.10:2121/img.bin flash:` |
| SFTP     | yes (SSH) | user/password      | opt-in  | `copy sftp://user@192.168.1.10:2222/img.bin flash:` |
| SCP      | yes (SSH) | user/password      | opt-in  | `copy scp://user@192.168.1.10:2222/img.bin flash:` |
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
  `state/secrets.toml`. Passwords are intentionally displayed unmasked in the
  TUI so you can type them into a switch console; they are **never** written to
  logs or `config.toml`.
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
- **No secrets in logs or config:** passwords live only in a `0600` secrets
  file; TLS keys and SSH host keys are stored with `0600` as well.
- **Rate limiting:** per-IP lockout after repeated failed logins.
- **Limits & timeouts:** configurable max parallel sessions, idle session
  timeouts, upload size limit.
- **Warnings:** binding to all interfaces and enabling cleartext protocols is
  called out in the logs and the TUI.
- **No telemetry.** transferbuddy makes no outbound connections.

## HTTPS certificates

On first HTTPS start a self-signed certificate is generated (valid for
`localhost` and all current local IPs) and stored under
`certificates/cert.pem` / `key.pem`. The Services view shows the SHA-256
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
`ssh/host_ed25519_key` (`0600`). The Services view shows its fingerprint. If a
device refuses to connect after you recreated the key, clear the old entry on
the client (`ssh-keygen -R "[host]:2222"` on macOS/Linux).

## Configuration

Persistent state lives in:

```
~/Library/Application Support/transferbuddy/
├── config.toml      # ports, binds, enabled services, upload settings, …
├── logs/            # transferbuddy.log (when file logging is on)
├── certificates/    # cert.pem, key.pem (self-signed, generated)
├── ssh/             # host_ed25519_key
└── state/           # secrets.toml (0600 — passwords, never in config.toml)
```

Settings changed in the TUI are saved automatically. CLI flags override the
stored configuration for the current run.

The shared root is deliberately **not** part of `config.toml`: transferbuddy
always serves the directory it was started in, unless `--root` says otherwise.
Starting it somewhere else therefore always shares that place, and a `root =`
key left over from an older version is ignored.

Beyond ports and services, `config.toml` also holds the UI preferences:

```toml
speed_in_bits = true   # show speeds in bit/s instead of byte/s
sound         = true   # beeps when toggling settings (m in the TUI)
intro         = true   # animated intro screen on start
```

## TUI keys

| Key | Action |
|-----|--------|
| `q` | quit (asks to stop running services) |
| `h` / `?` | help (two key tables, one key per line) |
| `m` | sound on/off |
| `Tab` / `1`–`6` | switch view |
| `f` / `a` / `l` / `c` / `w` | files / sessions / logs / services / switches |
| arrows / `j` `k` | navigate |
| `Enter` | open directory / show Cisco commands / select |
| `Space` | enable/disable service |
| `s` | start/stop service (Services) · cycle sort (Files) |
| `r` | restart service |
| `S` / `X` | start all enabled / stop all |
| `p` / `b` | change port / bind address |
| `n` / `w` | set username / password |
| `u` / `d` | toggle uploads / set upload directory (Services) |
| `d` | deploy the selected file to a switch over SSH (Files) |
| `/` | filter (files, logs) |
| `y` | copy the shown Cisco command to the clipboard |
| `B` | toggle bit/s ↔ byte/s (Sessions) |
| `G` | follow log tail |
| `H` | file hashes (MD5/SHA-256/SHA-512) with compare |
| `c` | cancel a running job (Switches) |
| `r` | re-read dir + show version (Switches) |
| `x` / `X` | disconnect / clear closed sessions (Switches) |
| `L` | log level (Logs) · logs of the selected service (Services) |
| `P` | protocol filter (Logs) |

## CLI reference

```
transferbuddy [OPTIONS]

--root <DIR>            directory to share (default: current directory)
--http --https --ftp --sftp --scp --tftp --all
                        enable services (SFTP/SCP share one SSH service)
--port-http <P> --port-https <P> --port-ftp <P> --port-sftp <P> --port-tftp <P>
--bind <ADDR>           bind address for all services (default 0.0.0.0)
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
| Device can't reach the server | check the bind address; the dashboard lists all local interfaces. VPN/tunnel interfaces are not used for suggestions |
| `copy tftp:` times out | IOS talks to port 69 only — run `sudo transferbuddy --tftp` |
| `copy https:` fails on the device | the self-signed certificate isn't trusted; install it as a trustpoint or use HTTP |
| SCP/SFTP host key error on device | the host key changed; clear the old known-host entry on the device |
| Upload rejected | uploads are disabled by default (`--uploads`), files are never overwritten, size limit may apply |
| Deploy says "… is not running" | the deploy only uses services you started — press `s` on the protocol in the services view |
| Deploy stops at "unexpected prompt" | the device asked something transferbuddy will not answer on its own; the transcript shows the question. Run that `copy` by hand |
| Deploy fails with "host key … changed" | remove the named line from `state/known_hosts` if the device really was replaced |
| Flash space shows `—` | the session has not read `dir` yet, or the device answered something unexpected — press `r` in the switches view |
| Ping always shows `—` | transferbuddy shells out to the system `ping`; a firewall dropping ICMP looks the same as a device being down |
| Wrong directory shared | the root always follows the working directory — check the dashboard's `root:` line, and `--root` if you passed it. Older builds pinned the root in `config.toml`; the stale `root =` key is now ignored, so re-installing is enough |

## Development

```bash
cargo run -- --http --no-tui --root ./images     # run from source
cargo test                                       # unit + integration tests
cargo fmt && cargo clippy                        # style & lints
```

The code is organized as independent modules around a small core:

```
src/
├── main.rs        # startup, headless mode
├── cli.rs         # clap argument parser
├── config.rs      # config.toml model, CLI merge, validation
├── deploy.rs      # the command whitelist: what may be typed on a device
├── switch.rs      # long-lived SSH sessions, device facts, ping monitor
├── fsroot.rs      # SecureRoot: path traversal & symlink jail
├── session.rs     # SessionManager: live transfer metrics
├── services/      # ServiceManager + one adapter per protocol
│   ├── http.rs    #   HTTP/HTTPS (reference implementation)
│   ├── ftp.rs     #   FTP (PASV/EPSV/PORT/EPRT, RETR/STOR/LIST)
│   ├── ssh.rs     #   SFTP + SCP on one russh-based service
│   └── tftp.rs    #   TFTP with blksize/tsize options
├── tui/           # ratatui views (no protocol dependencies)
│   ├── mod.rs     #   state, key handling
│   ├── views.rs   #   all six views and the modals
│   ├── theme.rs   #   Atari-flavoured palette and panel helpers
│   └── intro.rs   #   block-letter intro animation
├── sound.rs       # short beeps for TUI toggles
├── auth.rs        # credentials, per-IP lockout
├── certs.rs       # self-signed TLS certificates
├── sshkeys.rs     # SSH host key management
├── netif.rs       # interface detection & IP suggestion
├── cisco.rs       # copy-command generator, dir/show-version parsers
└── logging.rs     # structured log entries, ring buffer, file sink

testdata/          # recorded device output the parsers are tested against
```

Every protocol adapter implements the same lifecycle (`run(ctx)` with a
shutdown signal, session registration and metrics via `SessionManager`), so a
new protocol never touches the TUI or the session model.

### Release builds

```bash
# Apple Silicon
cargo build --release --target aarch64-apple-darwin
# Intel
rustup target add x86_64-apple-darwin
cargo build --release --target x86_64-apple-darwin
# Universal binary
lipo -create -output transferbuddy \
  target/aarch64-apple-darwin/release/transferbuddy \
  target/x86_64-apple-darwin/release/transferbuddy
```

For distribution outside Homebrew, sign and notarize:

```bash
codesign --sign "Developer ID Application: …" --options runtime transferbuddy
xcrun notarytool submit transferbuddy.zip --keychain-profile … --wait
```

## License

MIT
