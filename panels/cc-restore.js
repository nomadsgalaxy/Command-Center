// cc-restore puts every window cc-windows.js changed back the way it was. kwin.rs loads it once,
// with `const saved = {uuid: {noBorder, x, y, w, h, onAllDesktops, desktops}}` put in front of it
// (from ~/.cache/control-center/kwin-restore.json). Minimized is left however the user has it,
// since cc-windows.js never changes that on its own (an older file's `minimized` is ignored).
for (const w of workspace.windowList()) {
    const s = saved[w.internalId.toString()];
    if (!s)
        continue;
    w.noBorder = s.noBorder; // first, because the saved frame includes the border
    w.onAllDesktops = s.onAllDesktops;
    if (!s.onAllDesktops && s.desktops) // false alone means the current desktop
        w.desktops = workspace.desktops.filter(d => s.desktops.includes(d.id));
    w.frameGeometry = {x: s.x, y: s.y, width: s.w, height: s.h};
}
