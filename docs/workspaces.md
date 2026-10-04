# Workspaces (2026-10-03)

A workspace is one place's layout (its spots: home, scanned, presets) plus the machines that go
with it. They're kept in `~/.config/control-center/home.json` under `"workspaces"`. The rules live
in `crates/cc-proto/src/conf.rs`, which cc-panels and cc-home share, and they're unit-tested there.

## Dedicated workspaces and Temporary

There are two kinds:

- **Dedicated** workspaces are yours. You make one with *Save as workspace* (or `cc-home
  workspace save-as|new`). Every workspace that existed before Temporary (`default`, `home`,
  `workspace-2`, ...) also counts as dedicated, and I left those exactly as they were: never
  renamed or deleted. Each one is bound to the SteamVR room (tracking universe) it was made in, and
  to any room it was entered in later (see "Back at a workspace in a new room").
- **Temporary** (key `temporary`, shown as "Temporary") is the travel workspace. It's never bound
  to a room.

When cc-panels starts, it picks one with `conf::choose_workspace`:

1. A dedicated workspace bound to this room (any of its rooms): the active one if it is, else the first.
2. No room known (universe 0, or the Frame's dummy 424242/525252): the network's hint
   (`cc-home workspace-for`), else the active one.
3. A new room while the active workspace is dedicated and bound to no room yet (`cc-home
   workspace new`): that one, and it gets bound to this room.
4. Anything else (you're travelling, or there's no dedicated workspace yet): **Temporary**. If it
   doesn't exist, it's made from the active workspace's layout and machines. After that it keeps
   its own layout from trip to trip, since moving panels saves there. It no longer makes a
   `workspace-N` for every new room.

A fresh install (no home.json) starts in Temporary, so everything saves there until you make your
first dedicated workspace.

## Loading a workspace while travelling

In a room that isn't a workspace's own, the Workspace page offers **Load for travel** for it
(`cc-home workspace load <name>`).

The catch is that its spots are in its own room's standing space, so they can't just be applied
as they are. Instead they move as one rigid piece (`conf::load_workspace`, the same upright move
as `cc-home reanchor`):

- The anchor is its primary machine's first placed monitor, or else the middle and mean heading
  of everything placed.
- The anchor goes 1 m (`LOAD_AHEAD`) in front of you at your heading, at its own height above the
  floor, facing you.
- Every panel keeps its position, rotation, size and curve relative to the anchor.

Heights are measured above SteamVR's floor, so set the floor in a new room first. I saw this live
on 2026-10-03: the layout landed where I expected once the floor was calibrated.

The result is written to Temporary, with the workspace's machines and `"loaded": <name>`, and
Temporary becomes active. The loaded workspace itself (its spots and its room binding) is never
written, so its own room brings it back exactly as it was.

This is for travel only. A loaded layout isn't where the real monitors are, so it leaves out the
`scanned` and `before-align` spots, which describe the home room's real monitors. That means
snap-back and reanchor have nothing of the home room's to act on in Temporary. The active
workspace's dot shows which layout you're using.

## Back at a workspace in a new room

The Frame's inside-out tracking sometimes rebuilds its map, and SteamVR then gives the same room a
new universe id. I hit it coming back to my desk: same room, new id, so Command Center started in
Temporary. The workspace's spots are still right relative to each other, only the whole frame
moved. So one of its monitors is enough to bring it back.

It's on you, not automatic: the Workspace window's **Enter workspace** on that workspace's row (or
`cc-home workspace enter Home [monitor ...]`). It needs one of its monitors connected, since that's
what shows the tags. With none, the button is greyed and says to connect one of its machines first.

- Every connected monitor of it that has a saved scan there shows its four corner tags (distinct
  ids per monitor, through its agent's tag screen). No calibrating, no grid.
- Look at any one of them for a few seconds. The first one that gets 12 frames over 2 cm of head
  travel ends it, so whichever you look at is the root. Every monitor's saved pose is in the same
  frame, so any of them works.
- The solve fits it with its saved shape (flat, or its curve) and compares that with its saved
  "scanned" pose. Same size (3%, `conf::SAME_SIZE`) and the same tilt and roll (3 deg,
  `SAME_TILT`, since gravity doesn't change) means the same monitor. Any other monitor found has to
  land within 50 mm (`SAME_DESK_MM`) of where the root's move puts it.
- Then every spot of the workspace moves with it, as one upright rigid piece (yaw and a shift,
  `conf::rigid`, the same move as `cc-home reanchor`). The old home is kept as "previous", so
  `cc-home apply previous` undoes it.
- This room is added to the workspace, and it goes live. Next time this room picks it straight away.

If the fit is poor, it's a different monitor, or the monitors disagree, it says why and nothing
changes. You stay in Temporary.

Why four tags and not one: a single tag's yaw rests on a square about a tenth of the monitor's
width, and yaw is what swings the far end of the desk. In the solve's simulation
(`quick_corners_pin_the_desk` in cc-scan's solve.rs: 12 frames, 0.5 px of corner noise, 1 mm and
0.1 deg of head tracking noise), a point 1 m from the monitor landed within 8.6 mm with one tag,
2.3 mm with the four corners, and 1.7 mm with the full align's grid. That's a simulation, so I
expect the real thing to land near the full align's 2 to 6 mm, not better. I haven't measured it
live yet.

home.json keeps one room the way it always did, as `"universe": "<id>"`. A second room adds
`"universes": [first, second]`, and `"universe"` stays the first one, so an older build still knows
the original room. A room belongs to one workspace, so entering one takes that room off any other.

## Machines per workspace

Machines stay paired globally (viewers.conf, trusted-hosts). A workspace's `"machines"` lists
the machine ids in it (viewers.conf's `machine=`, else the host). **No list means every
machine**, so existing workspaces and Temporary behave the way they did before. The first edit
writes the list.

Only the active workspace's machines get panels and taskbar chips, and `cc-home autoconnect` lists
only them. Save as and Load carry the list along.

## The Workspace window (cc-panels, `control/workspace.rs`)

The taskbar chip **Workspace** (it used to be "Machines") opens it. There's a row per workspace,
Temporary first, showing a dot (solid when active), its name, where it belongs (this room / room
..1234 / for travelling / no room yet), how many panels its layout places, and these buttons:

- **Enter workspace** finds you at it in a new room (above). It's greyed when none of its monitors
  is connected, and Temporary has none. Progress shows on the status line.
- **Use** switches to it live: only its machines are connected, and every panel, the taskbar and
  the windows go to its spots. For a workspace bound to another room it's **Load for travel**
  instead, and the active one shows **Active** (greyed).
- **Machines** opens the Machines window for that workspace. Its title names the workspace and
  **Back** returns. Each row has an **In workspace** switch. Connect, auto-connect, align, rename
  and pairing work the same as before.
- **Rename** turns the name into a field. Type with SteamVR's keyboard or a physical one; Enter
  saves and Escape cancels. Temporary can't be renamed.
- **x** deletes it, and a second click confirms. It never deletes the active one.

Under the rows is **Save as workspace**: name it, and it makes a new dedicated workspace from the
current layout and machines, bound to this room, and makes it active.

From the command line:

- cc-panels' control socket takes `workspace show|hide|reload` and `universe`.
- `cc-home workspace [use|new|save-as|load|enter|rename|forget|machines <ws> [add|remove <machine>]]`.
  `enter <ws> [monitor ...] [--progress]` takes the monitors to show tags on (default: every one of
  its monitors with a saved scan there).
  Use, load and a change to the active workspace's machines tell a running cc-panels to
  `workspace reload`.

ponytail: the workspace is picked at start. A room change mid-session doesn't switch it; Use or
Load on the Workspace page does.
