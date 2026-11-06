# Agentlight

A USB desk dashboard for coding agents. The host helper is Rust on Linux; the
firmware is Arduino C++ for the **Waveshare ESP32-S3-Touch-LCD-5B**, the
1024 × 600 touch model (not the 800 × 480 5). Based on the ideas and serial
protocol of [managents](https://github.com/tonylook/managents).

Also supports encrypted local Wi-Fi, automatic mDNS discovery and explicit
per-display approval. USB remains available and takes priority while connected.

## Build and flash

Requirements: Rust/Cargo, Python, PlatformIO Core, a USB data cable and the
5B board. Building firmware downloads the ESP32 toolchain and dependencies.

```sh
cargo build --release
cargo test
cd firmware
pio run
pio run -t upload
```

If upload fails, hold BOOT while connecting the board, release BOOT, upload,
then press RESET. To select a port, add `--upload-port /dev/ttyACM0` to the
upload command. Linux users may need permission to access the serial device.
Stop the helper before flashing; both cannot use the port at once.

On the development machine PlatformIO is at
`~/.platformio/penv/bin/pio`; use that path if `pio` isn't on PATH. Its
Python environment must have compatible PlatformIO/esptool dependencies.

## Run

```sh
cargo run --release -- --demo       # synthetic cards, no agent access
cargo run --release                 # actual sessions
cargo run --release -- --port /dev/ttyACM0
cargo run -- --once                 # print one JSON frame, no USB required
```

Automatic discovery probes USB devices with Espressif's vendor ID (303a);
use `--port` if the device isn't discovered. A device must answer the
managents v1 hello handshake before it receives state. The helper reconnects
after a disconnect.

The top bar shows how many sessions are working or need you, the local time,
and whether the display is on USB or Wi-Fi. Each session is a card tinted by
status: working (green), needs you (amber), error (red) or idle (gray), with
its task and context progress and its subagents. With more than nine sessions,
swipe or tap the page dots to change pages; the display returns to the first
page after 30 seconds. Until the first update arrives, and again after six
seconds without one, a connection screen lists what the display is waiting for
(Wi-Fi, helper) and how to fix it.

## Sleep mode

Tap the moon in the top bar to save power, for example on a powerbank. The
screen turns off after 5 minutes without a touch, while the display keeps
listening for status over USB or Wi-Fi. The mode survives restarts and updates.

- A touch turns the screen back on and does nothing else. The 5 minutes then
  start again, and the mode stays on.
- When a session newly needs you (NEEDS YOU or ERROR), the screen turns on, any
  open log closes, and the moon becomes an amber **Sleep** button. The screen
  then stays on, even if you touch it, until that session's status changes or
  you tap **Sleep**. Either way the 5 minutes start again.
- Sessions already needing you when you turn the mode on, or when you tap
  **Sleep**, don't wake the screen unless they leave that status and return.
- To leave the mode, tap the highlighted moon. During an alert that takes two
  taps: **Sleep**, then the moon.

Claude Code and OpenCode sessions report NEEDS YOU for 2 hours after going idle,
then IDLE, so an alert nobody answers ends by itself after up to 2 hours. A
newly started session also counts, because it's waiting for you.

**A dark screen doesn't prove the helper is connected.** If the helper is lost
while the screen is off, it stays off to save power, and a touch shows the
connection screen. Sessions that started needing you meanwhile wake the screen
when the helper reconnects.

## Wi-Fi: setup requires explicit approval

The board supports **2.4 GHz Wi-Fi**. Initial setup is over a trusted physical
USB connection; it is not an open pairing web page or an unauthenticated Wi-Fi
pairing request. This also selects which helper to discover when several exist.

1. Flash the Wi-Fi firmware. Stop the running USB helper so setup can open its
   serial port (Ctrl+C if running it in your terminal).
2. Run, substituting your port:

   ```sh
   cargo run --release -- --wifi-setup --port /dev/ttyACM0
   ```

3. Check the displayed device ID. The program explains remote access and
   requires you to type **APPROVE**. Any other answer cancels. Then enter the
   network name and Wi-Fi password; the password is hidden and not put into
   shell arguments or source code. `--ssid` can optionally supply the network
   name, but never put your password in a command.
4. Start the helper:

   ```sh
   cargo run --release -- --wifi --wifi-only
   ```

   The display discovers the approved helper with mDNS (`_agentlight._tcp`),
   verifies its pinned certificate and authenticates. No IP entry is needed.
   Power the display from a charger to use it without a USB data connection.
   Use `--wifi` without `--wifi-only` to also serve connected USB displays.

The current transport is indicated by USB/Wi-Fi at the bottom left. Wi-Fi
reconnects automatically, and USB updates take priority when both are present.
The display stores its setup across power cycles.

```sh
cargo run --release -- --wifi-list
cargo run --release -- --wifi-revoke '28:84:85:A2:8F:C4'
```

Revocation disconnects active network access within a few seconds. It does not
disable physically connected USB access. To change Wi-Fi networks or helpers,
repeat USB setup and explicitly approve again.

### Wireless firmware updates

Approved Wi-Fi displays can be updated without a USB data connection:

```sh
cd firmware && pio run && cd ..
cargo run --release -- --wifi --wifi-only --ota firmware/.pio/build/waveshare_5b/firmware.bin
```

When an approved display connects with a different build, the helper installs
the image automatically. The display shows a full-screen progress bar with
size, percentage and time remaining, then restarts into the new firmware. The
helper prints a matching progress line. Each build is identified by
`FW_VERSION` in `firmware/platformio.ini` plus its compile time. A build
recompiles, and so counts as new, only when the source or build flags change.

- The first firmware with update support must be flashed once over USB
  (`pio run -t upload`). Older displays are skipped with a message.
- The image is downloaded into PSRAM and checked with SHA-256 before any flash
  is written. A failed or interrupted transfer leaves the current firmware
  running.
- Writing flash stalls the panel's PSRAM reads and makes the picture jump, so
  the screen shows a short notice and goes dark for that step (a few seconds)
  before restarting. The download itself shows a steady progress bar.
- A new image must reach an approved helper within 3 minutes of booting. If
  it doesn't, the display rolls back to the previous firmware. An image that
  hangs very early in boot rolls back on the next power cycle. The helper
  stops retrying a display after 2 attempts per run.
- Updates are accepted only over the pinned, approved helper connection.
  Images are not code-signed (ESP32 Secure Boot is not enabled), so anyone who
  controls the approved helper account can install firmware.

### Network security and limitations

- Remote access is **off unless the helper is started with `--wifi`**. Discovery
  advertises the helper identity, not session data; discovering a display never
  approves it. There is no remote auto-approve path.
- A separate random 256-bit credential is issued for each approved display.
  The display verifies the server certificate's SHA-256 pin **before** sending
  that credential. A spoofed mDNS reply cannot establish trust. Encryption and
  pinning do not depend on Internet access or an NTP clock.
- Approval grants status/task/subagent access and requested log excerpts. Treat
  the physical display as a trusted device: people nearby can read its screen.
- Helper keys and permissions live under `$XDG_CONFIG_HOME/agentlight` (normally
  `~/.config/agentlight`), with directory mode 0700 and file mode 0600. Do not
  publish those files. Losing the helper identity requires USB re-pairing.
- The display's Wi-Fi password and credential are stored in **unencrypted NVS**;
  this is not protection against someone physically reading its flash. Firmware
  does not log them. Factory flash encryption/secure boot is not configured.
- Discovery needs UDP 5353 multicast on the LAN; the TLS server listens on TCP
  **47832**. Client isolation, guest Wi-Fi or a firewall can prevent discovery
  or connection. No firewall rules or port forwarding are changed automatically.
  Do not expose this experimental service to the public Internet.
- The helper limits simultaneous network connections to two. Current tests
  cover TLS, rejection, authorized status/log access and revocation, but Wi-Fi
  provisioning and sustained display operation need testing on your network.

## Detection and privacy

This is a first implementation, not full managents parity:

- Claude Code: live `~/.claude/sessions` registry and process liveness; busy,
  waiting and idle states. Its registry `name` is shown as the activity line;
  the project is derived from the working directory (including worktrees).
  Running subagents from per-session metadata and recently finished or stopped
  subagents are shown within the parent card. When Claude's task list is available,
  the card shows completed/total tasks and a progress bar. Shared lists configured
  via `CLAUDE_CODE_TASK_LIST_ID` are used when visible in the process environment.
  Registry PID reuse is not currently checked.
  Parked terminal placeholders and idle detached background jobs are excluded;
  background jobs attached to an open terminal, working, or waiting for input
  are retained. Distinct active sessions are not merged by project directory.
- OpenCode: live processes and the most recent message for each open TUI tab
  from the OpenCode 1 or 2 SQLite database. Tab titles are displayed as the
  activity line; they are session titles, not necessarily live tool activity.
  Direct child sessions with recorded busy/idle times appear as subagents.
  Without tab state, a folder gets one fallback card and no nested children.
- Codex: live processes, the latest thread for the working directory, and
  task events in the end of its rollout. Saved session names (or titles on older
  versions) are displayed as the activity line. Internal subagent threads are
  excluded from this selection. Direct subworkers are linked by their recorded
  parent thread, including workers in other worktrees. Their task names and
  rollout lifecycle events supply the child labels and running/completed status;
  completed, interrupted and failed workers expire after two minutes.
  Approval prompts are not detected, and a stale prior rollout can give a
  misleading status. Context usage is
  not sent by the helper yet.

Agent data formats change; unreadable or unavailable sources fall back to a
waiting card where possible. The helper reads metadata from local state. By
default it uses only USB; `--wifi` enables LAN discovery and encrypted remote
access for explicitly approved devices. Agent IDs, project names, status, timing,
Claude registry names, OpenCode tab titles and Codex session names/titles go over
USB. These activity/title fields may contain user-provided or conversation-derived
text and are visible on the display; subagent names and descriptions are also sent.
Full paths are not sent.
**Tapping a session explicitly requests its recent messages, which then go over
USB and are visible on the screen.** Excerpts can contain sensitive user or
assistant text; there is no automatic secret redaction. Reasoning blocks and
tool arguments/results are omitted. Codex status detection scans backwards in
256 KiB chunks for lifecycle events; log excerpts read only the last 256 KiB.
Environment variables
`CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `XDG_DATA_HOME` select data directories.

The firmware accepts the subset of the [managents v1 protocol](https://github.com/tonylook/managents/blob/main/docs/protocol.md)
used here, including optional context bars. Full protocol validation, automatic
backlight dimming, multi-display broadcasting and service installation are not
implemented. Automatic discovery opens candidate serial ports to probe them;
use `--port` to target one device explicitly.

Up to six subagents appear per parent as pills; completed children remain for
approximately two minutes. Their times are based on the last available local
metadata timestamp and may not represent their exact start time.

Tap a session card, or one of its subagent pills, to open that conversation's
live log. Messages appear chat style (you, the agent, and tool calls as small
chips). While open, the display requests updates every two seconds and follows
the newest message. Drag to scroll back, which pauses following; tap "Jump to
latest" or the Paused badge to resume, and Back to return. This shows
persisted messages, not a terminal screen or complete raw log: at most 30
text/tool-name entries that fit one 4 KB frame; an oldest entry that does not
fit is trimmed at its start and marked `...`. Common Unicode punctuation is
converted to ASCII for the display font. Claude Code sessions and subagents and
OpenCode 2 tabs and child sessions are supported; Codex uses the same
latest-thread-per-folder selection as its status detector and supports direct
subworker logs. Unavailable sources show an explanation. Nothing is read for a log until
requested, and device requests must identify a currently sent session or
subagent.

## Hardware validation

The firmware builds against the 5B board configuration from
ESP32_Display_Panel (ST7262 RGB, GT911 touch, CH422G I/O expander). It uses a
two PSRAM panel framebuffers and swaps them at refresh boundaries, rather than
copying into the active scanout buffer. RGB runs at a conservative 14 MHz with
20-line internal bounce buffers to leave bandwidth for Wi-Fi. Before relying on
it, test on the actual board: colors, touch, USB reconnection, longer operation
and RGB drift/tearing. Build success alone does not verify these behaviors.
