# Command Center roadmap

Command Center is an add-on for the Steam Frame's **stock** desktop: the Frame keeps its own
SteamOS desktop (one nested Plasma session in gamescope); Command Center adds panels for other
machines' monitors, its own pointer and keyboard, and its settings. Nothing about the stock
desktop is replaced.

## What it covers

| Area | Command Center |
|---|---|
| The local desktop | The stock desktop stays; our pointer drives its panel too |
| Panel controls | Our own controls on our panels, Nomads Galaxy styled: move bar (scroll for distance), resize, curve, roll, wrist pin |
| Layouts | Spots (done), plus hide/show, reset, visibility modes and what happens when a game starts |
| 3D pointer | Across our panels and the stock desktop panel; the SteamVR dashboard and Steam UI through a virtual-controller driver that never takes a hand role from the user's controllers |
| Input | Our own input service: devices read directly, hotplug and reconnect, volume keys, button mapping |
| Settings | One settings app, which is also the device manager |
| Setup | Our own installer and build container |

## Phases

1. **Stand-alone.** Own installer and container (`install.sh`, `cc-box`); own input service
   (devices read directly, hotplug). Test on the stock desktop with nothing else relaying
   input.
2. **KVM essentials**: clipboard across machines; same-machine window drag across a machine's
   panels (KWin over D-Bus while the button is held).
3. **Overlay UI, panel controls and device manager**: our own widgets in a DMA-BUF overlay
   (Nomads Galaxy styled); move/distance/resize/curve/roll, wrist pin, hide/show, reset,
   visibility modes, layouts; the portrait's top-to-bottom curve; the device manager (add a
   machine, pick monitors, status, wake, input settings).
4. **3D pointer**: a room-anchored cursor moving in 3D between our panels and the stock
   desktop's panel (gamescope through a virtual pointer).
5. **SteamVR dashboard and Steam UI** for the pointer, never taking a hand role.
6. **Audio and cross-system drag**: each machine's sound streamed, then spatial; browser
   windows hand off by URL; other programs pop out as their own panel on the Frame.

Also: home laptop and Steam Deck (Sunshine) as machines; send the krdp pointer patch to KDE.
