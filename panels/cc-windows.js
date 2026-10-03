// cc-windows is KWin's side of Command Center's window panels (crates/cc-panels/src/kwin.rs and
// windows.rs). It reports every program window to cc-panels over the session bus and carries out
// cc-panels' commands. A KWin script can only call out, so it long-polls for those.
// Before it changes a window it reports how the window was ("saved"), so it can be restored on
// exit. If a call fails (cc-panels is gone), the polling ends, because callDBus drops the
// callback on an error. After that nothing gets adopted, since a "saved" nobody hears would
// make that window's changes stick.
// Plasma's own panel and its popups get reported as they are ("shell", plasmabar.rs) and never
// adopted. Minimizing belongs to the user (Plasma's taskbar, cc-panels' v and chips): it's
// reported, never undone here, and never put back on exit.
const SVC = "org.controlcenter.Panels";

function send(o) {
    callDBus(SVC, "/", SVC, "Event", JSON.stringify(o));
}

function id(w) {
    return w.internalId.toString();
}

const adopted = {}; // uuid -> window
const shells = {};  // uuid -> "panel" or "popup", plasmashell's windows as reported
let heard = Date.now(); // last Next() reply, and cc-panels answers within 10 s

// Never shown as panels: the shell, and cc-view's own (paused) RDP viewers (R-3). Their class is
// cc-view-<name> (cc-view's /wm-class) in the Flatpak com.freerdp.FreeRDP.
const IGNORED = ["plasmashell", "org.kde.plasmashell", "krunner", "org.kde.krunner"];

// "window", "dialog", or null for one we leave alone. Popups, menus and tooltips come later (S3b).
function kind(w) {
    const cls = (w.resourceClass || "").toLowerCase();
    const app = (w.desktopFileName || "").toLowerCase();
    if (!w.managed || w.deleted || IGNORED.includes(cls) || cls.startsWith("cc-view-") || cls.startsWith("xfreerdp")
        || app === "com.freerdp.freerdp" || app.startsWith("xfreerdp"))
        return null;
    if (w.desktopWindow || w.dock || w.notification || w.onScreenDisplay || w.popupWindow || w.splash)
        return null;
    if (w.dialog || (w.transient && w.transientFor))
        return "dialog";
    const g = w.clientGeometry;
    if (w.normalWindow && (!w.skipTaskbar || (g.width >= 200 && g.height >= 150)))
        return "window";
    return null;
}

function screen(name) {
    return workspace.screens.find(s => s.name === name);
}

// Puts the window at the top-left of Virtual-1, or Virtual-0 if it only fits there, and never on
// Virtual-2 (the pointer's park). cw, ch are the client size wanted (0 keeps it as it is).
function place(w, cw, ch) {
    if (w.fullScreen)
        return; // the app's own full screen, the panel just follows it
    const f = w.frameGeometry, c = w.clientGeometry;
    const W = (cw || c.width) + f.width - c.width, H = (ch || c.height) + f.height - c.height;
    // In the Desktop's session (cc-home session), Virtual-1 holds the windows. Virtual-0 is Plasma's panel and its popups,
    // so they never cover a window, and Virtual-2 is the pointer's park.
    const order = ["Virtual-1", "Virtual-0"];
    const outs = order.map(screen).filter(s => s);
    const area = s => workspace.clientArea(KWin.MaximizeArea, s, workspace.currentDesktop);
    const out = outs.find(s => area(s).width >= W && area(s).height >= H) || outs[0] || workspace.screens[0];
    const a = area(out);
    w.frameGeometry = {x: a.x, y: a.y, width: Math.min(W, a.width), height: Math.min(H, a.height)};
}

function report(w, e) {
    const g = w.clientGeometry;
    send({e: e, uuid: id(w), kind: kind(w), app: w.desktopFileName || w.resourceClass, caption: w.caption,
          parent: w.transientFor ? id(w.transientFor) : "", x: g.x, y: g.y, w: g.width, h: g.height,
          minimized: w.minimized});
}

// Sorts plasmashell's windows: its panel ("panel", the dock), the windows it opens like the
// launcher, the tray, the calendar and menus ("popup"), and tooltips and task previews
// ("tooltip"). Its desktops, notifications and OSDs get null.
function shell(w) {
    const cls = (w.resourceClass || "").toLowerCase();
    if ((cls !== "plasmashell" && cls !== "org.kde.plasmashell") || w.desktopWindow || w.notification
        || w.criticalNotification || w.onScreenDisplay)
        return null;
    return w.dock ? "panel" : w.tooltip ? "tooltip" : "popup";
}

function shellReport(w, shown) {
    const g = w.frameGeometry;
    send({e: "shell", uuid: id(w), role: shells[id(w)], shown: shown && !w.hidden, x: g.x, y: g.y, w: g.width, h: g.height});
}

function watchShell(w) {
    const role = shell(w);
    if (!role || shells[id(w)])
        return;
    shells[id(w)] = role;
    w.frameGeometryChanged.connect(() => shellReport(w, true));
    if (w.hiddenChanged)
        w.hiddenChanged.connect(() => shellReport(w, true));
    shellReport(w, true);
}

function adopt(w) {
    watchShell(w);
    const k = kind(w);
    if (!k || Date.now() - heard > 12000)
        return;
    const u = id(w);
    if (!adopted[u]) {
        const f = w.frameGeometry;
        send({e: "saved", uuid: u, s: {noBorder: w.noBorder, x: f.x, y: f.y, w: f.width, h: f.height,
                                       onAllDesktops: w.onAllDesktops, desktops: w.desktops.map(d => d.id)}});
        adopted[u] = w;
        // when it's minimized its panel hides (the stream would go blank), and shows again when it's back
        w.minimizedChanged.connect(() => send({e: "minimized", uuid: u, on: w.minimized}));
        w.clientGeometryChanged.connect(() => report(w, "geom"));
        w.captionChanged.connect(() => send({e: "caption", uuid: u, caption: w.caption}));
        // the app's own full screen (a video's, F11), which cc-panels puts in theater mode
        w.fullScreenChanged.connect(() => send({e: "full", uuid: u, on: w.fullScreen}));
        w.onAllDesktops = true;
        w.noBorder = true; // just saves output space (it only affects server-side decorations)
        place(w, 0, 0);
    }
    report(w, "add");
}

function run(c) {
    const w = adopted[c.uuid];
    if (!w)
        return;
    if (c.c === "place") {
        place(w, c.w, c.h);
    } else if (c.c === "raise") {
        // cc-panels holds a press until this answers, so nothing is above w when the press goes through
        workspace.raiseWindow(w);
        send({e: "raised", uuid: c.uuid, n: c.n});
    } else if (c.c === "activate") {
        w.minimized = false;
        workspace.activeWindow = w; // a taskbar chip: unminimize, raise, and give it the keyboard
    } else if (c.c === "minimize" || c.c === "unminimize") {
        w.minimized = c.c === "minimize"; // a v or a chip (its panel already hid), or summon
    } else if (c.c === "close") {
        w.closeWindow(); // same as its own close button, so it may ask to save first
    } else if (c.c === "nudge") {
        // an idle window draws nothing, so its stream never gets a first frame. Make it a pixel wider,
        // then back
        const f = w.frameGeometry;
        w.frameGeometry = {x: f.x, y: f.y, width: f.width + c.d, height: f.height};
    }
}

function poll() {
    callDBus(SVC, "/", SVC, "Next", function (s) {
        heard = Date.now();
        if (s) {
            for (const c of JSON.parse(s))
                run(c);
        }
        poll();
    });
}

send({e: "hello"});
for (const w of workspace.windowList())
    adopt(w);
send({e: "ready"});
workspace.windowAdded.connect(adopt);
// any activation might cover the window cc-panels last raised
workspace.windowActivated.connect(w => { if (w) send({e: "activated", uuid: id(w)}); });
workspace.windowRemoved.connect(w => {
    const u = id(w);
    if (shells[u]) {
        shellReport(w, false);
        delete shells[u];
    }
    if (adopted[u]) {
        delete adopted[u];
        send({e: "remove", uuid: u});
    }
});
poll();
