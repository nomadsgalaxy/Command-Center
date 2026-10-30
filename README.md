<img src="packaging/command-center.png" alt="" width="96">

# Command Center

Command Center is a VR desktop for the Steam Frame. Your windows, and the monitors of your other
computers, hang in the room around you as panels you can move, resize and curve. One mouse and
keyboard drive whichever panel you point at. It's all Rust. More at
[framecc.nomadsgalaxy.com](https://framecc.nomadsgalaxy.com).

## What you need

- A Steam Frame with SteamOS and SteamVR.
- A Bluetooth mouse and keyboard paired with the Frame. For now they're required.
- Computers running Linux with KDE Plasma 6 on Wayland and krdp, on the same local network.
  A Steam Deck in desktop mode counts.

## Install

**Heads up: there's no release yet.** The one-line installer below downloads from the latest
GitHub release, and there isn't one, so for now it stops with "Couldn't download the installer".
Until the first release, the Frame side installs from a checkout:

```
git clone https://github.com/nomadsgalaxy/Command-Center ~/control-center
~/control-center/install.sh            # the build container, cc-home, then cc-home install
~/control-center/install.sh desktop    # the Command Center tile in the VR launcher
```

The computer side (cc-host) comes from the release too, so it waits for the first one.

Once there's a release, run this in a terminal on the Frame, then on each computer you want to
see from it:

```
curl -fsSL https://framecc.nomadsgalaxy.com/install | sh
```

The installer works out which machine it's on. Run it again later to update or remove Command
Center.

- **On the Frame,** it builds Command Center from a checkout in `~/control-center`, the same way
  `install.sh` does. The first run takes a while. When it's done, open
  **Command Center** from the SteamVR launcher. It has its own tile, next to SteamOS's Desktop,
  which stays as it was. Removing Command Center takes the tile away and nothing else.
- **On a computer,** you pick which monitors the Frame can show. If something would stop the
  Frame from finding or reaching the computer, like the firewall, the installer shows the exact
  `sudo` commands to fix it and asks first. The details are in
  [docs/packaging.md](docs/packaging.md#host-fixes-what-the-installer-finds-and-fixes).

With no terminal to answer questions on, pass the answers: `| sh -s -- --yes` installs or
updates, `--remove --yes` removes, and `--dry-run` only says what it would do.

## Pair and align

1. On the computer, run `cc-share pair`. A 6-digit key fills its screen.
2. On the Frame, open **Workspace → Machines → Add machine**. Pick the computer, or just press
   **Pair by looking** and look at its screen: the key screen carries the computer's address too,
   so it doesn't need to be in the list. You can also type the address and key.
3. Press **Align**. The monitors show AprilTags, the headset's camera finds them, and each panel
   lands on its real monitor.

## Everyday use

- **Spots** save where your panels are in the room. `cc-home save desk` saves them, and
  `cc-home apply desk` puts them back.
- **Workspaces** keep a layout per room. Back in a room, **Enter workspace** uses one of its
  monitors to put everything where it belongs.
- **Closing the Desktop** saves which apps were open and frees the memory for a game. Opening
  it again brings them back.
- **Games:** while a game runs, Command Center is built to step aside. Its panels hide and pause,
  and the controllers, mouse and keyboard all go to the game. That's meant for VR games and for
  flat Steam games, the ones SteamVR shows on a floating screen. During a flat game you should be
  able to get Command Center back without quitting it: open SteamVR's dashboard and pick the
  Command Center tab. Choosing **Steam / game** there hands everything back to the game. When the
  game ends, whatever it hid comes back. This is all in the code, but it hasn't passed a headset
  test yet.
- **The dashboard tab:** open SteamVR's dashboard and Command Center has its own tab, next to
  Steam's, even while the Desktop is closed (then Show opens it). It shows or hides the Desktop, and it lets you choose where your mouse and keyboard go:
  Command Center, or Steam and your game. SteamVR puts its own bar under the tab, and its close
  button quits Command Center, the way it quits any overlay app
  ([docs/steamvr-integration.md](docs/steamvr-integration.md)).
- **Moving windows:** grab a panel's bar or edge with the trigger to carry it, the thumbstick
  pushes it away or pulls it closer, the bottom-right corner resizes it, a tap on the curve button
  curves it, and (with `"docking": true`) holding it at your other hand or your face until it buzzes docks it there.
  Docking is off by default: Command Center moves a docked panel once a frame, which can trail a
  fast hand, and it stays off until SteamVR moves it itself (a device-relative transform)
  ([docs/steamvr-integration.md](docs/steamvr-integration.md)).
- **Right click with the shoulder button:** the Frame controller's bumper is right click on
  SteamVR's laser, so context menus are one press away.
- **Typing with a controller:** the keyboard button on a panel's bar opens SteamVR's keyboard,
  and what you type goes to that computer or window. Text fields in Command Center's own windows
  open it too, unless you've just been typing on a real keyboard. It has no function keys, and
  window panels and some RDP servers get a US keyboard's keys, so a character a US keyboard
  doesn't have isn't sent. It sends characters, not keys, so a game's WASD won't see it. The
  details are in [docs/steamvr-integration.md](docs/steamvr-integration.md#steamvrs-keyboard).
  The typing and the shortcuts (Ctrl, Alt, Super on our key strip) haven't been tested in the
  headset yet.
- **Notifications:** pairing results, errors, and a computer's screen sharing going down or
  coming back show as notifications in the headset. `"notifications": false` in settings.json
  turns them off.

On the Frame, `cc-home update` updates Command Center's system image to the latest release
without a password, after checking the release's signature
([docs/packaging-frame.md](docs/packaging-frame.md#updates-without-a-password-after-that)). That's
for the system image, which the installer doesn't install yet (it's built by hand with
`packaging/sysext`), and with no release yet there's nothing for it to install. A checkout install
updates by running the installer, or `git pull` and `install.sh`, again.

On a computer, `cc-share check` says if anything's wrong and how to fix it, and `cc-share frames`
lists the Frames paired with it.

## Security

- The Frame and a computer only talk after pairing, through the computer's agent (TLS on port
  3399) and RDP (ports 3400 and up).
- The installer opens those ports for private networks only, and never in an untrusted firewall
  zone.
- With the system image, any program running as the user at the headset can start `cc-home
  update` without a password. It can only install the latest release whose signature checks out,
  never an older version or one it picks.
- A computer's krdp gets the Frame's password through `--password-file`, which our patched krdp
  takes. A distro's krdp doesn't, krdp 6.8 included, since our patches don't build against 6.8
  yet, so there the password goes on krdp's command line, where other local users can read it.
  `cc-share check` lists that as a need ([docs/packaging.md](docs/packaging.md#the-krdp-patches-stay-as-they-are)).

## AI Usage
I leaned on AI to help me write a decent amount of this, being unfamiliar with coding in a 3D space. I make sure that it isn't making decisions for me, and that I am understanding the systems it makes to the best of my ability.

## Where things stand

Each feature above is at one of three levels. **Device-tested** means it was tried on the Frame or
a real computer, on the date given; later changes may not have been tried again. **Reviewed**
means it's built, has tests, and the code has been reviewed, but there's no headset or host test
on record. **Built** means it's in the code and not yet past either.

| Feature | Where it is | Notes |
| --- | --- | --- |
| Pairing, and pair by looking | Device-tested, Oct 2 | The first live pairing was by looking. |
| Align | Device-tested, Oct 3 | The first Rust align landed 5 mm and 4 mm off. On Oct 24 SteamVR's mirror switched eyes and everything sat about 3 cm sideways; that's fixed by hand, and the code fix is still on a branch. |
| Mouse and keyboard on panels | Device-tested, Oct 3 | The mouse works with both controllers. The Bluetooth keyboard in Command Center's own windows is reviewed, not headset-checked. |
| Closing the Desktop (hibernate) | Device-tested, Oct 3 | One real cycle, with one app. |
| Workspaces window | Device-tested, Oct 3 | |
| Spots and workspaces (`cc-home save`, `apply`) | Device-tested before the Rust port | The Rust version's final test (docs/final-test-frame.md) hasn't been filled in. |
| Moving, curving and dragging panels | Device-tested, Oct 2 | Resizing about the centre came later and is reviewed only. |
| Windows from a computer as their own panels | Reviewed | The first pop-out was tried on Oct 2: close worked, and right-click menus turned the panel white. Window panels were reworked since, and that version isn't on record as tested. |
| Plasma's taskbar and start menu | Device-tested, Oct 2 | Open bug, Oct 29: with the start menu open, a laser lands right across it but too near its middle up and down, one or two items low at the top. A laser now goes where its own ray meets the menu, the way the mouse does; that hasn't been tried in the headset yet. |
| Steam Deck as a computer | Device-tested, Oct 4 | With the fixed krdp from `krdp/deck/build.sh`. |
| Audio from a computer | Device-tested, Oct 4 | |
| Clipboard between machines | Device-tested, Oct 1 (text) | Files over the clipboard were tried on a branch on Oct 4. That experiment has been taken off the hosts since, so files aren't in. |
| Notifications | Reviewed | |
| Right click with the bumper | Reviewed | |
| Typing with SteamVR's keyboard, and its shortcuts | Reviewed | Not tested in the headset yet. |
| Games stepping aside, and the dashboard tab | Reviewed | Not tested in the headset yet. |
| Docking | Reviewed, off by default | See above. The headset checklist in docs/steamvr-integration.md is still open. |
| VNC screens | Reviewed | The first connect and a pinned reconnect are untested, since there's no VNC host to try them on. |
| The system image and `cc-home update` | Reviewed | On Oct 29, picking the newest image (vpick) and the passwordless polkit check were tried on the Frame with a throwaway image. A full update from a signed release, and the first live merge of the real image, haven't been done. |
| The one-line installer | Built | Waits for the first release. |

One known gap in `cc-home update`: if SteamVR starts the dashboard tab's process in the few
milliseconds between the helper's check and the refresh, the refresh happens under it
([docs/packaging-frame.md](docs/packaging-frame.md#updates-without-a-password-after-that)).

## Not yet

- **Controlling a Steam Deck out of the box:** SteamOS's own krdp shows the screen but drops
  mouse and keyboard input. `krdp/deck/build.sh` builds a fixed one
  ([docs/packaging.md](docs/packaging.md#steamos-hosts)); the installer doesn't do it for you yet.
- **Gaming-grade latency:** RDP suits desktop work, not fast games.

## Changing the code

Every push and pull request to `main` on GitHub runs CI: format, clippy, the tests on x86_64 and
aarch64, and a license and advisory check. It doesn't block a merge yet, since `main` has no
branch protection, and cc-panels' job doesn't fail the run. [docs/ci.md](docs/ci.md) says what it
checks and how to run it before you push.

## License

OCL v1.1 + SWAtt v1 (Open Community License v1.1 + Software Attribution v1, by Prusa Research).
Creator: Nomads Galaxy. See [NOTICE.md](NOTICE.md), [LICENSE](LICENSE) and
[LICENSE-SWAtt-v1.md](LICENSE-SWAtt-v1.md). Third-party components keep their own licenses.
