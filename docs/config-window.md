# Config window (proposal; I chose a native VR overlay, D-010)

This started as a proposal for one settings window in the headset. Most of it is built now, in
cc-panels rather than a separate app, so I'll say where it stands first and keep the proposal
below for the reasoning.

## Where it stands

**2026-10-02.** I built v1 of the Machines screen in cc-panels
(`crates/cc-panels/src/control/machines.rs`). The taskbar chip or `machines show|hide` opens it.
Each row has the connected dot, name and host, plus:

- **[Connect]/[Disconnect]** (`viewer disconnect <name>` does the same).
- **[Auto-connect]**, which runs `cc-home machine set <name> autoconnect=yes|no`. It applies at
  the next Desktop launch.
- **[Align]**, which runs `cc-home machine align <name> --progress` on the host through
  distrobox-host-exec, since the scan needs podman and the container doesn't have it.

**2026-10-03.** The taskbar chip is now **Workspace**. The Workspace window lists the saved
workspaces (Use / Load for travel, Machines, Rename, delete, Save as workspace) and opens the
Machines window per workspace ([workspaces.md](workspaces.md)).

What I listed as "not yet" on 2026-10-02 has mostly landed since, according to the code:

- **Add machine** and **Remove** are there: [Add machine] opens a form (address, monitor, size,
  name) and [x] removes a row on a second click. The form also finds hosts announcing themselves
  and pairs them ([pairing.md](pairing.md)).
- **Carrying and resizing** the window work: it's carried, resized, bent and closed the way a
  panel is (grab.rs `Extra`).
- **The Workspace strip** became the Workspace window above.
- **Align's live steps** show in the headset as the scan's own HUD (`control/hud.rs`): a status
  strip with the current step, and green outlines on the tags it's reading. The Machines window
  itself still only sends Align's `@progress` lines to the log.

## The flow

The flow I want: **add a krdp machine → choose auto-connect on Desktop launch → "Align"
it on the real monitors with AprilTags → saved to a workspace.**

## Data (no new files)

I didn't want a new config file for this, so the window only edits what already exists:

- `viewers.conf` gets an option `autoconnect=yes|no`, using the same `key=value` tail as
  `curve=`. If it's absent you get today's behaviour, so nothing changes until you flip it.
- The alignment result is the existing spot `scanned` in the active workspace (`home.json`), plus
  `vcurve` for a top-to-bottom curve.
- The window edits only these two. cc-home stays the one writer of `home.json`.

## Screens

The proposal planned these as overlay widgets (list, button, toggle, text field):

1. **Machines**: one row per viewers.conf entry, with name, host, connected dot, an
   [Auto-connect] toggle, [Align] and [Remove]. [Add machine] at the bottom.
2. **Add machine**: name, user@host, monitor index (port 3400+i), size, curve (flat/h/v). It
   writes the line only. Installing krdp on that machine (`cc-share install`) stays a separate
   step.
3. **Align**: needs the camera and the monitor's tags. It shows the guided steps as text in the
   overlay while the monitor shows its tags, places the result live and saves it to the active
   workspace.
4. **Workspace** strip: the active workspace's name, [New] and [Switch] (cc-home workspace).

## cc-panels commands this needed

At the time of the proposal none of these existed: `viewers` (a list, one line each including
autoconnect), `viewer add|set|remove ...` (rewrites viewers.conf), and `align <name>` (hides the
panels, runs `cc-home scan <name>`, shows them again and reports progress lines), plus the
already-requested `head` and `tag show/hide`. As built, the window runs `cc-home machine
set|add|remove` itself and Align goes through `cc-home machine align`.

## Dependencies and limits

- Auto-connect: cc-panels has to skip `autoconnect=no` machines at start and connect them on
  demand from the window.
- Align takes the head pose from cc-panels' `head`. The scan asks each machine's agent for its
  monitor and tags (`crates/cc-home/src/scan.rs`, `ready`).
- I'd planned an Overlay UI Toolkit (Cairo + Pango) for the widgets, with the same operations as
  `cc-home machine add|set|list|align` commands in the meantime, so the window would only be a
  front end. The commands came first as planned. The windows themselves ended up drawn by
  cc-panels directly (the theme's colours, its own glyphs in `assets.rs`), with no toolkit.
