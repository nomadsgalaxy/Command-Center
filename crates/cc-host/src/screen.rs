//! Shows a drawn Canvas full screen on one output. This is the Wayland side of draw.rs: a layer-shell
//! overlay over everything that takes the keyboard (Esc, Enter, Shift). It's pure Rust, with no
//! libwayland and no xkb (it reads evdev key codes), so the static binary needs nothing on the host.
//! The buffer is in the output's native pixels (fractional scale) and is shown 1:1 through a viewport.
use crate::draw::Canvas;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::fractional_scale::v1::client::{wp_fractional_scale_manager_v1 as fsm, wp_fractional_scale_v1 as fs};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1 as shell, zwlr_layer_surface_v1 as layer};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Key {
    Esc,
    ShiftEsc,
    Enter,
}

#[derive(Debug, PartialEq)]
pub enum Ev {
    Key(Key),
    /// The native size changed, so redraw at size().
    Resized,
    /// The compositor closed the surface, or the connection went away.
    Closed,
}

#[derive(Default)]
struct St {
    outputs: Vec<(wl_output::WlOutput, String)>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    shift: [bool; 2],
    logical: (u32, u32),
    /// The preferred scale, in 120ths.
    scale: u32,
    configured: bool,
    events: Vec<Ev>,
}

pub struct Screen {
    queue: EventQueue<St>,
    st: St,
    surface: wl_surface::WlSurface,
    layer: layer::ZwlrLayerSurfaceV1,
    viewport: wp_viewport::WpViewport,
    shm: wl_shm::WlShm,
    shown: Option<wl_buffer::WlBuffer>,
}

impl Screen {
    /// Opens full screen on the named output (KWin's names, the same as kscreen-doctor's). For None,
    /// the compositor picks, which means the active output.
    pub fn open(output: Option<&str>) -> Result<Screen, String> {
        let conn = Connection::connect_to_env().map_err(|e| {
            // Say which socket it tried, so a missing one shows up in the agent's journal.
            let disp = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0 (WAYLAND_DISPLAY unset)".into());
            let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "(XDG_RUNTIME_DIR unset)".into());
            format!("no Wayland display at {run}/{disp}: {e}")
        })?;
        let (globals, mut queue) = registry_queue_init::<St>(&conn).map_err(|e| format!("Wayland registry: {e}"))?;
        let qh = queue.handle();
        let mut st = St { scale: 120, ..St::default() };
        fn missing(what: &'static str) -> impl Fn(wayland_client::globals::BindError) -> String {
            move |e| format!("the compositor has no {what}: {e}")
        }
        let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 4..=6, ()).map_err(missing("wl_compositor"))?;
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(missing("wl_shm"))?;
        let shell: shell::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).map_err(missing("layer shell"))?;
        let viewporter: wp_viewporter::WpViewporter = globals.bind(&qh, 1..=1, ()).map_err(missing("viewporter"))?;
        let fractional: Option<fsm::WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let _seat: Option<wl_seat::WlSeat> = globals.bind(&qh, 1..=7, ()).ok();
        for g in globals.contents().clone_list().into_iter().filter(|g| g.interface == wl_output::WlOutput::interface().name && g.version >= 4) {
            let o: wl_output::WlOutput = globals.registry().bind(g.name, 4, &qh, st.outputs.len());
            st.outputs.push((o, String::new()));
        }
        queue.roundtrip(&mut st).map_err(|e| format!("Wayland: {e}"))?;
        let on = match output {
            None => None,
            Some(name) => Some(st.outputs.iter().find(|(_, n)| n == name).map(|(o, _)| o.clone()).ok_or_else(|| {
                format!("no screen {name}: {}", st.outputs.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(" "))
            })?),
        };
        let surface = compositor.create_surface(&qh, ());
        let viewport = viewporter.get_viewport(&surface, &qh, ());
        if let Some(f) = &fractional {
            f.get_fractional_scale(&surface, &qh, ());
        }
        let layer = shell.get_layer_surface(&surface, on.as_ref(), shell::Layer::Overlay, "command-center".into(), &qh, ());
        layer.set_anchor(layer::Anchor::Top | layer::Anchor::Bottom | layer::Anchor::Left | layer::Anchor::Right);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(layer::KeyboardInteractivity::Exclusive);
        surface.commit();
        while !st.configured {
            queue.blocking_dispatch(&mut st).map_err(|e| format!("Wayland: {e}"))?;
            if st.events.contains(&Ev::Closed) {
                return Err("the compositor closed the screen".into());
            }
        }
        st.events.clear();
        Ok(Screen { queue, st, surface, layer, viewport, shm, shown: None })
    }

    /// The output's size in native pixels. Draw at this size.
    pub fn size(&self) -> (usize, usize) {
        let (w, h) = self.st.logical;
        let n = |v: u32| ((v * self.st.scale + 60) / 120) as usize;
        (n(w), n(h))
    }

    /// Shows a canvas. Any size works because it's scaled to the whole output, and at size() it's 1:1.
    pub fn show(&mut self, c: &Canvas) -> Result<(), String> {
        let bytes: Vec<u8> = c.px.iter().flat_map(|p| p.to_le_bytes()).collect();
        let fd = unsafe { libc::memfd_create(c"cc-host-screen".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(format!("memfd: {}", std::io::Error::last_os_error()));
        }
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        file.write_all(&bytes).map_err(|e| format!("memfd: {e}"))?;
        let qh = self.queue.handle();
        let pool = self.shm.create_pool(std::os::fd::AsFd::as_fd(&file), bytes.len() as i32, &qh, ());
        let buf = pool.create_buffer(0, c.w as i32, c.h as i32, c.w as i32 * 4, wl_shm::Format::Xrgb8888, &qh, ());
        pool.destroy();
        self.surface.attach(Some(&buf), 0, 0);
        self.viewport.set_destination(self.st.logical.0 as i32, self.st.logical.1 as i32);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.surface.commit();
        // The old buffer is KWin's until it sends release, and Dispatch below destroys it then.
        // Destroying it here, before that, made a second picture end the screen (live, 2026-10-03:
        // the align's tags went away when capture showed its grid).
        self.shown = Some(buf);
        self.queue.flush().map_err(|e| format!("Wayland: {e}"))
    }

    /// Waits up to `ms` and returns what happened: keys, a new size, or the end.
    pub fn wait(&mut self, ms: i32) -> Vec<Ev> {
        let ok = self.queue.flush().is_ok() && self.queue.dispatch_pending(&mut self.st).is_ok();
        if ok && self.st.events.is_empty()
            && let Some(guard) = self.queue.prepare_read()
        {
            let mut p = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut p, 1, ms) } > 0 && let Err(e) = guard.read() {
                eprintln!("cc-host tagscreen: Wayland read: {e}");
                self.st.events.push(Ev::Closed);
            }
        }
        if !ok {
            eprintln!("cc-host tagscreen: Wayland connection lost");
            self.st.events.push(Ev::Closed);
        } else if let Err(e) = self.queue.dispatch_pending(&mut self.st) {
            eprintln!("cc-host tagscreen: Wayland: {e}");
            self.st.events.push(Ev::Closed);
        }
        std::mem::take(&mut self.st.events)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.layer.destroy();
        self.surface.destroy();
        let _ = self.queue.flush();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for St {
    fn event(_: &mut St, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<St>) {}
}

impl Dispatch<wl_output::WlOutput, usize> for St {
    fn event(st: &mut St, _: &wl_output::WlOutput, ev: wl_output::Event, i: &usize, _: &Connection, _: &QueueHandle<St>) {
        if let wl_output::Event::Name { name } = ev {
            st.outputs[*i].1 = name;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for St {
    fn event(st: &mut St, seat: &wl_seat::WlSeat, ev: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<St>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = ev
            && c.contains(wl_seat::Capability::Keyboard)
            && st.keyboard.is_none()
        {
            st.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

// evdev codes, from linux/input-event-codes.h.
const KEY_ESC: u32 = 1;
const KEY_ENTER: u32 = 28;
const KEY_KPENTER: u32 = 96;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;

impl Dispatch<wl_keyboard::WlKeyboard, ()> for St {
    fn event(st: &mut St, _: &wl_keyboard::WlKeyboard, ev: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<St>) {
        // The keymap's fd gets dropped with the event, which closes it.
        if let wl_keyboard::Event::Key { key, state: WEnum::Value(state), .. } = ev {
            let down = state == wl_keyboard::KeyState::Pressed;
            match key {
                KEY_LEFTSHIFT => st.shift[0] = down,
                KEY_RIGHTSHIFT => st.shift[1] = down,
                KEY_ESC if down => st.events.push(Ev::Key(if st.shift.contains(&true) { Key::ShiftEsc } else { Key::Esc })),
                KEY_ENTER | KEY_KPENTER if down => st.events.push(Ev::Key(Key::Enter)),
                _ => {}
            }
        }
    }
}

impl Dispatch<layer::ZwlrLayerSurfaceV1, ()> for St {
    fn event(st: &mut St, l: &layer::ZwlrLayerSurfaceV1, ev: layer::Event, _: &(), _: &Connection, _: &QueueHandle<St>) {
        match ev {
            layer::Event::Configure { serial, width, height } => {
                l.ack_configure(serial);
                if st.configured && (width, height) != st.logical {
                    st.events.push(Ev::Resized);
                }
                st.logical = (width.max(1), height.max(1));
                st.configured = true;
            }
            layer::Event::Closed => st.events.push(Ev::Closed),
            _ => {}
        }
    }
}

impl Dispatch<fs::WpFractionalScaleV1, ()> for St {
    fn event(st: &mut St, _: &fs::WpFractionalScaleV1, ev: fs::Event, _: &(), _: &Connection, _: &QueueHandle<St>) {
        if let fs::Event::PreferredScale { scale } = ev
            && scale != st.scale
        {
            st.scale = scale.max(1);
            st.events.push(Ev::Resized);
        }
    }
}

delegate_noop!(St: wl_compositor::WlCompositor);
delegate_noop!(St: ignore wl_surface::WlSurface);
delegate_noop!(St: ignore wl_shm::WlShm);
delegate_noop!(St: wl_shm_pool::WlShmPool);
impl Dispatch<wl_buffer::WlBuffer, ()> for St {
    fn event(_: &mut St, b: &wl_buffer::WlBuffer, ev: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<St>) {
        if let wl_buffer::Event::Release = ev {
            b.destroy();
        }
    }
}
delegate_noop!(St: shell::ZwlrLayerShellV1);
delegate_noop!(St: wp_viewporter::WpViewporter);
delegate_noop!(St: wp_viewport::WpViewport);
delegate_noop!(St: fsm::WpFractionalScaleManagerV1);

/// The tag screen's rules, taken from tagshow.py: what it does on each line from the agent, each key
/// and each tick. Esc answers "escaped" and only cancels, because an accidental Esc shouldn't block a
/// Frame. It shows a light grey screen for 1 s, and a second Esc during that (or Shift+Esc) asks
/// whether to block this Frame, with Enter answering "block". --ask puts up the same question right
/// away (the agent's, after 3 cancels).
#[derive(Default)]
pub struct Flow {
    cancelled_at: Option<f64>,
    ask_until: Option<f64>,
}

pub enum In {
    Line(serde_json::Value),
    Eof,
    Key(Key),
    Tick,
}

#[derive(Debug, PartialEq)]
pub enum View {
    Tags(serde_json::Value),
    Quiet,
    Ask(String),
}

#[derive(Debug, Default, PartialEq)]
pub struct Out {
    pub print: Option<&'static str>,
    pub view: Option<View>,
    pub quit: bool,
}

pub const BLOCK_Q: &str = "Block this Frame for 10 minutes?\nEnter = block, Esc = no";
const ASK_S: f64 = 20.0;

impl Flow {
    pub fn asking(text: &str, now: f64) -> (Flow, View) {
        (Flow { ask_until: Some(now + ASK_S), ..Flow::default() }, View::Ask(text.into()))
    }

    pub fn step(&mut self, input: In, now: f64) -> Out {
        let quit = |print| Out { print, quit: true, ..Out::default() };
        if let Some(until) = self.ask_until {
            return match input {
                In::Key(Key::Enter) => quit(Some("block")),
                In::Key(_) => quit(Some("allow")),
                In::Tick if now >= until => quit(Some("allow")),
                _ => Out::default(),
            };
        }
        let ask = |me: &mut Flow, print| {
            me.ask_until = Some(now + ASK_S);
            Out { print, view: Some(View::Ask(BLOCK_Q.into())), quit: false }
        };
        match (input, self.cancelled_at) {
            (In::Line(p), None) => Out { print: Some("ok"), view: Some(View::Tags(p)), quit: false },
            (In::Eof, None) => quit(None),
            (In::Key(Key::Esc), None) => {
                self.cancelled_at = Some(now);
                Out { print: Some("escaped"), view: Some(View::Quiet), quit: false }
            }
            (In::Key(Key::ShiftEsc), None) => {
                self.cancelled_at = Some(now);
                ask(self, Some("escaped"))
            }
            (In::Key(Key::Esc | Key::ShiftEsc), Some(_)) => ask(self, None),
            (In::Tick, Some(t)) if now - t >= 1.0 => quit(None),
            _ => Out::default(),
        }
    }
}

/// cc-host tagscreen <output> [--ask <text>]. It does what tagshow.py --params / --ask did, drawn here.
pub fn tag_screen_main(output: &str, ask: Option<String>) -> i32 {
    use std::io::BufRead;
    let mut screen = match Screen::open(Some(output)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cc-host tagscreen: {e}");
            return 1;
        }
    };
    let now = crate::platform::mono;
    let (mut flow, mut view) = match &ask {
        Some(text) => {
            let (f, v) = Flow::asking(text, now());
            (f, Some(v))
        }
        None => (Flow::default(), None),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    if ask.is_none() {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                let _ = tx.send(serde_json::from_str(&line).ok());
            }
        });
    }
    let mut eof_sent = false;
    let mut redraw = view.is_some();
    loop {
        if redraw && let Some(v) = &view {
            let (w, h) = screen.size();
            let c = match v {
                View::Tags(p) => {
                    let s = &p["size"];
                    let (pw, ph) = (s[0].as_u64().unwrap_or(w as u64) as usize, s[1].as_u64().unwrap_or(h as u64) as usize);
                    crate::draw::tag_screen(p, pw.clamp(1, 16384), ph.clamp(1, 16384))
                }
                View::Quiet => crate::draw::quiet(w, h),
                View::Ask(text) => crate::draw::prompt(text, w, h),
            };
            if let Err(e) = screen.show(&c) {
                eprintln!("cc-host tagscreen: {e}");
                return 1;
            }
        }
        redraw = false;
        let mut inputs: Vec<In> = vec![In::Tick];
        for ev in screen.wait(50) {
            match ev {
                Ev::Key(k) => inputs.push(In::Key(k)),
                Ev::Resized => redraw = true,
                Ev::Closed => return 0,
            }
        }
        loop {
            match rx.try_recv() {
                Ok(Some(p)) => inputs.push(In::Line(p)),
                Ok(None) => {} // The agent checks what it sends, so a bad line is just skipped.
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if ask.is_none() && !eof_sent {
                        eof_sent = true;
                        inputs.push(In::Eof);
                    }
                    break;
                }
            }
        }
        for i in inputs {
            let out = flow.step(i, now());
            if let Some(p) = out.print {
                println!("{p}");
            }
            if let Some(v) = out.view {
                view = Some(v);
                redraw = true;
            }
            if out.quit {
                return 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn esc_only_cancels_and_a_second_asks() {
        let mut f = Flow::default();
        assert_eq!(f.step(In::Line(json!({"tags": []})), 0.0).print, Some("ok"));
        let o = f.step(In::Key(Key::Esc), 1.0);
        assert_eq!((o.print, o.view, o.quit), (Some("escaped"), Some(View::Quiet), false));
        assert_eq!(f.step(In::Eof, 1.1), Out::default()); // The agent closes stdin right away, but the 1 s still stays.
        assert!(f.step(In::Line(json!({})), 1.2).view.is_none()); // Nothing more gets drawn.
        assert_eq!(f.step(In::Key(Key::Esc), 1.5).view, Some(View::Ask(BLOCK_Q.into())));
        assert!(!f.step(In::Tick, 3.0).quit); // The question waits for an answer.
        assert_eq!(f.step(In::Key(Key::Enter), 4.0), Out { print: Some("block"), view: None, quit: true });
    }

    #[test]
    fn a_lone_esc_ends_after_a_second() {
        let mut f = Flow::default();
        f.step(In::Key(Key::Esc), 0.0);
        assert!(!f.step(In::Tick, 0.5).quit);
        assert_eq!(f.step(In::Tick, 1.0), Out { print: None, view: None, quit: true });
        let mut f = Flow::default();
        assert!(f.step(In::Eof, 0.0).quit); // EOF before any Esc means the agent hid it.
    }

    #[test]
    fn shift_esc_and_ask() {
        let mut f = Flow::default();
        let o = f.step(In::Key(Key::ShiftEsc), 0.0);
        assert_eq!((o.print, o.view), (Some("escaped"), Some(View::Ask(BLOCK_Q.into()))));
        assert_eq!(f.step(In::Key(Key::Esc), 1.0).print, Some("allow"));
        let (mut f, v) = Flow::asking("block?", 0.0);
        assert_eq!(v, View::Ask("block?".into()));
        assert!(!f.step(In::Tick, 19.0).quit);
        assert_eq!(f.step(In::Tick, 20.0), Out { print: Some("allow"), view: None, quit: true });
    }
}
