# Working like SteamOS

Command Center tries to behave like the rest of SteamVR on the Frame: the same bar, keyboard,
notifications and window handling you get from Valve's own windows, wherever SteamVR gives an
app a way to use them. This page says what we use, where, and how to turn it off.

## SteamVR's control bar

SteamVR draws its own bar, with a close button, under Command Center's tab in the dashboard.
Close quits Command Center, the same as quitting any other overlay app from the dashboard.

The floating windows (remote screens, window panels, pop-outs, and the Machines, Preferences and
Workspace windows) keep our own bar, buttons and card: move, resize, curve, dock, face me and the
rest. That's because SteamVR only draws its bar for what its dashboard hosts. Valve's header says
the bar is "drawn underneath of it in the dashboard", and SteamVR's dashboard code only reads the
close and keyboard flags from its own frames. A window we place in the room isn't one of those.

To turn the bar off, put this in `~/.config/control-center/settings.json` and restart the Desktop:

```json
{ "steamvr_bar": false }
```
