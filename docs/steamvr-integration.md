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

## SteamVR's keyboard

Every live panel (a computer's screen, a window, a pop-out) has a keyboard button on its bar,
right of the curve button and, on a window, the zoom button. Press it and SteamVR's keyboard
opens for that panel. Each key goes to the computer or window the moment you press it, the same
way a real keyboard's keys go, and Backspace and Enter work too. Press the button on another panel
to type there instead.

What it can't do:

- SteamVR's keyboard has no Esc, arrow keys, Ctrl, Alt or function keys. Use a real keyboard for
  those.
- It types as a US keyboard does. A character that isn't on one (an accented letter, an emoji)
  isn't sent.
- Press a key on a real keyboard while it's open and it closes, since you've gone back to the
  real one.

The text fields in the Machines and Workspace windows open SteamVR's keyboard when you click
them. If you've pressed a key on a real keyboard in the last minute, they don't, and the real
keyboard types into the field instead.

## Notifications

Command Center tells you in the headset when:

- a pairing worked, or didn't, and why;
- something in the Machines window failed (adding, removing or renaming a machine);
- a computer stopped sharing a screen (it gave up restarting it), keeps restarting it, or is back;
- a different computer answers at a paired machine's address, so it needs pairing again.

They're ordinary desktop notifications, sent to Steam's own notification service on the Frame
(org.freedesktop.Notifications on your user bus), the same way other apps on the device notify
you. If that service doesn't answer, the note shows as a SteamVR toast instead.

They don't pile up. One thing, like a screen's health, says something at most every 30 seconds,
and if it changes again meanwhile you get only the latest. No more than four go out in a minute.

To turn them off, add this to `~/.config/control-center/settings.json` (it applies straight away):

```json
{ "notifications": false }
```

Whether they show in the headset hasn't been checked on every SteamOS version yet. To try it,
open the Desktop and run `cc-home notify-test` for a notification, or `cc-home notify-test toast`
for SteamVR's toast on its own.
