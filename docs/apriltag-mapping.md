# AprilTag monitor mapping

This started on the `apriltag` branch, which is merged now.

`cc-home scan` finds each remote monitor through the VR mirror camera: the monitors show ArUco
4x4 tags, the camera reads them, and the result is saved in the spot `scanned`. Then
`cc-home apply scanned` places the panels.

- **Head pose** comes from cc-panels' `head` command on `@controlcenter`
  (`ok <12 floats, 3x4 row-major> <ms>`), not from cc-tip. Every cc-tip run was an OpenVR
  connect, and a scan would need hundreds of them, which would burn through the binding-load
  budget.
- **The mirror camera's fit** is `~/.config/control-center/mirror-camera.json`. A first fit or a
  refit needs tags at known poses. `cc-home refit` does that with a board of tags on one of our
  own overlays, 1 m ahead, looked at from three head positions; it keeps the new fit only if it's
  good, and keeps the old one as `mirror-camera.json.bak`.
- **Curve:** panels only bend left to right (`curve`, in metres). A top-to-bottom curve is saved
  as `vcurve` in the spot, and isn't sent to `place`.
- **Tags** are shown through each machine's agent (cc-host).

## Workspaces and the primary machine (my call, 2026-10-02)

Temporary (travel), the room rules, loading a workspace abroad and machines per workspace are in
[workspaces.md](workspaces.md) (2026-10-03).

Each workspace has one **primary machine**, the anchor of that place: home → the home desktop,
work → the work laptop (that desk has its own setup).

- A machine has one or more monitors (viewers.conf lines). The machine is the `machine=<name>`
  option on the line, or else the host, so today `203.0.113.63` = desk-wide + desk-portrait.
- Other machines get brought in as needed and float. Only the primary's monitors are matched to
  the real ones by Align.
- `cc-home workspace primary <machine>` sets it (`-` clears it). It's stored as `"primary"` in the
  workspace in home.json (cc-panels keeps keys it doesn't know).
- Removing a machine's last monitor clears it wherever it was primary.
- A new workspace starts with no primary, since it's a different place.
- `cc-home machine list` groups monitors by machine.

## Known networks: autoconnect and the workspace fallback (my call, 2026-10-02)

A workspace lists its **known networks** (`cc-home workspace known-network add|remove <name>`,
`"known_networks"` in the workspace). A network is a wifi name or, for a hardlined headset, the
wired connection's NetworkManager name. You choose them (for example home: `Home`, `Office`;
work: `Work-WiFi`). Nothing gets added to the real config automatically.

- **Autoconnect is opt-in.** My call: only if a user decides it, because some people use
  CC without krdp. Only monitors marked `autoconnect=yes` connect at launch, and only on one of
  the workspace's known networks (a workspace with no list isn't gated). If it isn't set, it's
  no. The primary machine doesn't imply connecting; it's the alignment anchor. When travelling
  there are lots of networks and krdp goes over a VPN, so you connect from the config window.
  `cc-home autoconnect [--all] [name ...]` prints them. `--all` prints
  `workspace<TAB>monitor,monitor` for every workspace, since the launcher doesn't know yet which
  workspace SteamVR will pick. Empty output with exit 0 means connect none; failures exit
  non-zero.
- **Workspace fallback:** the SteamVR room (universe) picks the workspace. When SteamVR can't tell
  where you are (universe 0), the current network does: `cc-home workspace-for [name ...]` prints
  the active workspace if it knows the network, or else the only one that does, or else nothing.
  It never guesses between two.
- `cc-home network` shows the headset's networks and which workspaces know them. One limit: a
  wired connection name like "Wired connection 1" is generic and may repeat between places. A
  gateway address would be a firmer id if that ever bites.

## Scan HUD (my call, 2026-10-02)

The passthrough cameras can't read text on the monitors, so the scan's instructions are in the
headset. cc-panels shows them with `hud` / `hud mark` / `hud hide` (control/hud.rs), and
cc-scan's hud.rs draws them:

- **A status strip,** head-locked 1 m ahead and 0.6 m down, tilted 31° up towards your eyes and
  0.5 m wide. That puts it below what the mirror camera sees (about 27° down), so it can't hide
  tags from the scan, but a glance down reads it. It shows the step, "n tags read" or "slower",
  and a laser click on its button skips the step.
- **Green outlines on the tags being read,** on a sheet placed in the room where those tags are.
- Both go away 5 s after the last refresh, so a scan that died doesn't leave them up.

The monitors show only tags: `frame.png` is the corner tags with the size ladder between them (the
corners count as its top rung), then `dense.png`, and `wait.png` on any monitor not being scanned.
There are no look/move/done/status cards.

**Big monitors:** the first step no longer needs all four corners in one frame. Three still frames
in a row with all four corners, or any 4 of the monitor's corner and ladder tags, are enough. The
fit is joint over every frame at known tag positions, so a monitor too big to see whole
(desk-wide, 1.19 m at 0.8 m) gets taken in parts.

## End-to-end Align

`Align` (config window) = `cc-home machine align <name>` = scan. The panels hide, the monitor
shows tags and the HUD guides you, frames are grabbed while your head is still, cc-scan's solver
fits position, size and curve, and the result is saved in `scanned` of the active workspace and
placed.

## Headset check, batched (needs me in the headset)

1. No scan: `cc-home apply scanned`. Do the panels sit on the real monitors? (workspace-2 may be
   off if the room origin moved.) If not, `cc-home workspace` shows the rooms, and a rescan
   fixes it.
2. Room view on, then `cc-home machine align desk-wide`, then `desk-portrait`. For each monitor,
   follow the HUD low in your view: look at the monitor and hold still until it has enough tags,
   then move your head to a new spot, pause, and repeat for about 25 s. Report the printed
   `fit N mm` (under 10 mm gets placed) and whether the panel sits over the monitor.
