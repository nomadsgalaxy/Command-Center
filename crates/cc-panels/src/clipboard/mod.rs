//! One clipboard for the Frame session and every machine. Copy anywhere and the hub announces
//! it everywhere else: a format list to one channel of each other machine (cliprdr.rs) and a
//! selection in the Frame session (frame.rs). Nothing moves until something gets pasted, then
//! the hub fetches it from wherever it was copied and keeps it for the next paste.
//!
//! Two exceptions to "nothing moves". A machine's text gets fetched as soon as it's announced,
//! because that's how echoes are told apart: each monitor is its own krdpserver on the same
//! host clipboard, so a copy there (or our own paste landing there) gets announced once per
//! monitor, and comparing the text is the one check that can't go wrong. Copies without text
//! (just files, say) fall back to time: the same formats from a machine we told less than
//! ECHO_TTL ago are its echo. The other one: krdp fetches text as soon as we announce it, so
//! a Frame copy goes to every machine right away. That's krdp's choice, and text is small.
//!
//! The formats are the Frame session's: UTF-8 text, an HTML fragment, a BMP file and a
//! text/uri-list. formats.rs turns them into RDP's. Files from a machine stay its
//! FileGroupDescriptorW until something pastes them, and then they're fetched into a staging
//! folder here (cliprdr.rs) and pasted as a uri-list.
pub mod cliprdr;
pub mod formats;
pub mod frame;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub static HUB: Mutex<Hub> = Mutex::new(Hub::new());

/// How long after we announce something to a machine its own announcements of the same
/// formats are taken for our echo.
pub const ECHO_TTL: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fmt {
    Text,
    Html,
    Image,
    Files,
}

/// Whose clipboard the current content came from. A machine is one clipboard, however many
/// monitors (channels) it has here.
#[derive(Clone, Debug, PartialEq)]
pub enum Owner {
    Frame,
    Machine(String),
}

/// Who's pasting: a machine's channel (its panel) or a Frame app (a token for its pipe).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Want {
    Rdp(usize),
    Frame(u64),
}

/// A connected clipboard channel: its panel, its machine, and whether it takes files.
#[derive(Clone, Debug, PartialEq)]
pub struct Chan {
    pub panel: usize,
    pub machine: String,
    pub files: bool,
}

pub type Data = Option<Arc<[u8]>>;

#[derive(Debug, PartialEq)]
pub enum Act {
    /// Announce these formats on this channel.
    Offer(usize, Vec<Fmt>),
    /// Make these formats the Frame session's selection.
    OfferFrame(Vec<Fmt>),
    /// Ask this channel for a format. Its answer comes back through remote_data.
    FetchRdp(usize, Fmt),
    /// Read a format from the Frame selection. The answer comes back through frame_data(copy, ..).
    FetchFrame(u64, Fmt),
    /// Hand a paste its data (None: we don't have it). For files from a machine the data is
    /// its descriptor and `src` the channel their contents come from.
    Give { to: Want, fmt: Fmt, data: Data, src: Option<usize>, copy: u64 },
}

struct Ask {
    panel: usize,
    fmt: Fmt,
    copy: Option<u64>, // None: text fetched to compare an announcement (which then carries these)
    machine: String,
    fmts: Vec<Fmt>,
}

pub struct Hub {
    owner: Option<Owner>,
    copy: u64, // goes up with each new copy, so late answers for an older one get dropped
    fmts: Vec<Fmt>,
    src: Option<usize>, // the owner machine's channel we fetch from
    data: Vec<(Fmt, Arc<[u8]>)>,
    adopted: Option<Instant>,
    told: Vec<(String, Instant)>, // machines that got the current content
    asked: Vec<Ask>,              // requests out on channels, oldest first (cliprdr answers in order)
    fetching: Vec<Fmt>,
    waiting: Vec<(Want, Fmt)>,
}

impl Hub {
    pub const fn new() -> Hub {
        Hub { owner: None, copy: 0, fmts: Vec::new(), src: None, data: Vec::new(), adopted: None, told: Vec::new(), asked: Vec::new(), fetching: Vec::new(), waiting: Vec::new() }
    }

    #[cfg(test)]
    pub fn owner(&self) -> Option<&Owner> {
        self.owner.as_ref()
    }

    #[cfg(test)]
    pub fn copy(&self) -> u64 {
        self.copy
    }

    fn cached(&self, f: Fmt) -> Data {
        self.data.iter().find(|(g, _)| *g == f).map(|(_, d)| d.clone())
    }

    /// A machine's channel announced a copy.
    pub fn remote_announce(&mut self, panel: usize, machine: &str, fmts: Vec<Fmt>, chans: &[Chan], now: Instant) -> Vec<Act> {
        if fmts.is_empty() {
            return vec![];
        }
        if fmts.contains(&Fmt::Text) {
            self.asked.push(Ask { panel, fmt: Fmt::Text, copy: None, machine: machine.into(), fmts });
            return vec![Act::FetchRdp(panel, Fmt::Text)];
        }
        let recent = |t: Option<&Instant>| t.is_some_and(|t| now.duration_since(*t) < ECHO_TTL);
        let told = recent(self.told.iter().find(|(m, _)| m == machine).map(|(_, t)| t));
        let same = fmts.iter().all(|f| self.fmts.contains(f));
        let its_own = self.owner == Some(Owner::Machine(machine.into())) && recent(self.adopted.as_ref());
        if same && (told || its_own) {
            return vec![]; // our paste echoed by another of its monitors, or its copy announced on each of them
        }
        self.adopt(Owner::Machine(machine.into()), fmts, Some(panel), vec![], chans, now)
    }

    /// A channel answered a request (None: it failed).
    pub fn remote_data(&mut self, panel: usize, bytes: Option<Vec<u8>>, chans: &[Chan], now: Instant) -> Vec<Act> {
        let Some(i) = self.asked.iter().position(|a| a.panel == panel) else { return vec![] };
        let ask = self.asked.remove(i);
        let bytes = bytes.filter(|b| !b.is_empty());
        let Some(copy) = ask.copy else {
            // the text of an announcement: new, or what we already hold coming back
            if bytes.is_some() && bytes.as_deref() == self.cached(Fmt::Text).as_deref() {
                return vec![];
            }
            let mut fmts = ask.fmts;
            let data = match bytes {
                Some(b) => vec![(Fmt::Text, Arc::from(b))],
                None => {
                    fmts.retain(|f| *f != Fmt::Text); // krdp announces empty text for anything it can't carry
                    if fmts.is_empty() {
                        return vec![];
                    }
                    vec![]
                }
            };
            return self.adopt(Owner::Machine(ask.machine), fmts, Some(panel), data, chans, now);
        };
        if copy != self.copy {
            return vec![]; // an older copy's
        }
        self.got(ask.fmt, bytes)
    }

    /// The Frame session has a new selection (not ours) with these formats.
    pub fn frame_announce(&mut self, fmts: Vec<Fmt>, chans: &[Chan], now: Instant) -> Vec<Act> {
        if fmts.is_empty() {
            return vec![];
        }
        self.adopt(Owner::Frame, fmts, None, vec![], chans, now)
    }

    /// The Frame selection's answer to FetchFrame(copy, fmt).
    pub fn frame_data(&mut self, copy: u64, fmt: Fmt, bytes: Option<Vec<u8>>) -> Vec<Act> {
        if copy != self.copy {
            return vec![];
        }
        self.got(fmt, bytes.filter(|b| !b.is_empty()))
    }

    fn got(&mut self, fmt: Fmt, bytes: Option<Vec<u8>>) -> Vec<Act> {
        self.fetching.retain(|f| *f != fmt);
        let data: Data = bytes.map(Arc::from);
        if let Some(d) = &data {
            self.data.push((fmt, d.clone()));
        }
        let (src, copy) = (self.src, self.copy);
        let (give, keep) = self.waiting.drain(..).partition(|(_, f)| *f == fmt);
        self.waiting = keep;
        give.into_iter().map(|(to, _)| Act::Give { to, fmt, data: data.clone(), src, copy }).collect()
    }

    /// Something got pasted: on a machine (krdp asks right after we announce) or in a Frame app.
    pub fn want(&mut self, to: Want, fmt: Fmt) -> Vec<Act> {
        let (src, copy) = (self.src, self.copy);
        if !self.fmts.contains(&fmt) {
            return vec![Act::Give { to, fmt, data: None, src, copy }];
        }
        if let Some(d) = self.cached(fmt) {
            return vec![Act::Give { to, fmt, data: Some(d), src, copy }];
        }
        self.waiting.push((to, fmt));
        if self.fetching.contains(&fmt) {
            return vec![]; // already on its way
        }
        self.fetching.push(fmt);
        match (&self.owner, self.src) {
            (Some(Owner::Frame), _) => vec![Act::FetchFrame(copy, fmt)],
            (Some(Owner::Machine(m)), Some(p)) => {
                let machine = m.clone();
                self.asked.push(Ask { panel: p, fmt, copy: Some(copy), machine, fmts: vec![] });
                vec![Act::FetchRdp(p, fmt)]
            }
            _ => self.got(fmt, None),
        }
    }

    /// A channel came up (MonitorReady): if its machine hasn't got the current content, it gets it.
    pub fn channel_up(&mut self, c: &Chan, now: Instant) -> Vec<Act> {
        if self.owner.is_none() || self.knows(&c.machine) {
            return vec![];
        }
        self.told.push((c.machine.clone(), now));
        offer(c, &self.fmts).into_iter().collect()
    }

    /// A channel went away: whatever it owed gets failed, and fetches move to another channel
    /// of its machine if there is one.
    pub fn channel_down(&mut self, panel: usize, chans: &[Chan]) -> Vec<Act> {
        if self.src == Some(panel) {
            let machine = chans.iter().find(|c| c.panel == panel).map(|c| c.machine.clone());
            self.src = machine.and_then(|m| chans.iter().find(|c| c.panel != panel && c.machine == m)).map(|c| c.panel);
        }
        self.waiting.retain(|(w, _)| *w != Want::Rdp(panel));
        let (gone, keep): (Vec<Ask>, Vec<Ask>) = std::mem::take(&mut self.asked).into_iter().partition(|a| a.panel == panel);
        self.asked = keep;
        let mut acts = Vec::new();
        let current = Some(self.copy);
        for a in gone.into_iter().filter(|a| a.copy == current) {
            acts.extend(self.got(a.fmt, None));
        }
        acts
    }

    fn knows(&self, machine: &str) -> bool {
        self.owner == Some(Owner::Machine(machine.into())) || self.told.iter().any(|(m, _)| m == machine)
    }

    fn adopt(&mut self, owner: Owner, fmts: Vec<Fmt>, src: Option<usize>, data: Vec<(Fmt, Arc<[u8]>)>, chans: &[Chan], now: Instant) -> Vec<Act> {
        let (old, copy) = (std::mem::take(&mut self.waiting), self.copy);
        let mut acts: Vec<Act> = old.into_iter().map(|(to, fmt)| Act::Give { to, fmt, data: None, src: self.src, copy }).collect();
        self.copy += 1;
        self.fetching.clear();
        self.told.clear();
        let from_machine = matches!(owner, Owner::Machine(_));
        (self.owner, self.fmts, self.src, self.data, self.adopted) = (Some(owner), fmts, src, data, Some(now));
        if from_machine {
            acts.push(Act::OfferFrame(self.fmts.clone()));
        }
        // one channel per machine: its monitors share one clipboard, and krdp fetches as soon as it's told
        for c in chans {
            if !self.knows(&c.machine) {
                self.told.push((c.machine.clone(), now));
                acts.extend(offer(c, &self.fmts));
            }
        }
        acts
    }
}

/// What a channel gets announced: files only if it said it takes them, and nothing if that's nothing.
fn offer(c: &Chan, fmts: &[Fmt]) -> Option<Act> {
    let f: Vec<Fmt> = fmts.iter().copied().filter(|f| *f != Fmt::Files || c.files).collect();
    (!f.is_empty()).then(|| Act::Offer(c.panel, f))
}

/// Carries out what the hub decided. Called without HUB held, since some of these call back in.
pub fn run(acts: Vec<Act>) {
    for a in acts {
        match a {
            Act::Offer(panel, fmts) => cliprdr::offer(panel, &fmts),
            Act::OfferFrame(fmts) => frame::offer(&fmts),
            Act::FetchRdp(panel, fmt) => cliprdr::fetch(panel, fmt),
            Act::FetchFrame(copy, fmt) => frame::fetch(copy, fmt),
            Act::Give { to: Want::Rdp(panel), fmt, data, src, .. } => cliprdr::give(panel, fmt, data, src),
            Act::Give { to: Want::Frame(token), fmt, data, src, copy } => frame::give(token, fmt, data, src, copy),
        }
    }
}

/// Starts the Frame session's side. The channels' side starts with each RDP session (rdp.rs).
pub fn start() {
    std::thread::spawn(frame::thread);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chans() -> Vec<Chan> {
        // the desktop has two monitors (panels 0 and 1), the laptop one (2) with no file support
        vec![
            Chan { panel: 0, machine: "desktop".into(), files: true },
            Chan { panel: 1, machine: "desktop".into(), files: true },
            Chan { panel: 2, machine: "laptop".into(), files: false },
        ]
    }

    fn text(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }

    fn arc(s: &str) -> Data {
        Some(Arc::from(s.as_bytes()))
    }

    #[test]
    fn a_machine_copy_goes_to_the_frame_and_one_channel_of_each_other_machine() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        assert_eq!(h.remote_announce(0, "desktop", vec![Fmt::Text], &c, t), vec![Act::FetchRdp(0, Fmt::Text)]);
        // the same copy announced on its other monitor too
        assert_eq!(h.remote_announce(1, "desktop", vec![Fmt::Text], &c, t), vec![Act::FetchRdp(1, Fmt::Text)]);
        assert_eq!(h.remote_data(0, text("hi"), &c, t), vec![Act::OfferFrame(vec![Fmt::Text]), Act::Offer(2, vec![Fmt::Text])]);
        assert_eq!(h.remote_data(1, text("hi"), &c, t), vec![]); // same text: nothing new
        assert_eq!(h.owner(), Some(&Owner::Machine("desktop".into())));
    }

    #[test]
    fn our_paste_echoed_by_a_machines_other_monitors_goes_nowhere() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        assert_eq!(h.frame_announce(vec![Fmt::Text], &c, t), vec![Act::Offer(0, vec![Fmt::Text]), Act::Offer(2, vec![Fmt::Text])]);
        // krdp on the desktop fetches it straight away, which fetches it from the Frame
        assert_eq!(h.want(Want::Rdp(0), Fmt::Text), vec![Act::FetchFrame(1, Fmt::Text)]);
        assert_eq!(h.frame_data(1, Fmt::Text, text("hi")), vec![Act::Give { to: Want::Rdp(0), fmt: Fmt::Text, data: arc("hi"), src: None, copy: 1 }]);
        // its second monitor's krdpserver sees the host clipboard change and announces it back
        assert_eq!(h.remote_announce(1, "desktop", vec![Fmt::Text], &c, t), vec![Act::FetchRdp(1, Fmt::Text)]);
        assert_eq!(h.remote_data(1, text("hi"), &c, t), vec![]);
        assert_eq!(h.owner(), Some(&Owner::Frame));
        // and it still counts after the echo window, since it's the text that matched
        assert_eq!(h.remote_announce(1, "desktop", vec![Fmt::Text], &c, t + ECHO_TTL * 2), vec![Act::FetchRdp(1, Fmt::Text)]);
        assert_eq!(h.remote_data(1, text("hi"), &c, t + ECHO_TTL * 2), vec![]);
    }

    #[test]
    fn frame_content_is_fetched_once_and_only_when_pasted() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        h.frame_announce(vec![Fmt::Text, Fmt::Image], &c, t);
        assert_eq!(h.want(Want::Rdp(2), Fmt::Image), vec![Act::FetchFrame(1, Fmt::Image)]);
        assert_eq!(h.want(Want::Rdp(0), Fmt::Image), vec![]); // already on its way
        let got = h.frame_data(1, Fmt::Image, text("BMP"));
        assert_eq!(got.len(), 2);
        assert_eq!(h.want(Want::Rdp(2), Fmt::Image), vec![Act::Give { to: Want::Rdp(2), fmt: Fmt::Image, data: arc("BMP"), src: None, copy: 1 }]);
        assert_eq!(h.want(Want::Rdp(2), Fmt::Html), vec![Act::Give { to: Want::Rdp(2), fmt: Fmt::Html, data: None, src: None, copy: 1 }]);
    }

    #[test]
    fn a_new_copy_fails_the_old_ones_pastes_and_drops_its_late_answers() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        h.frame_announce(vec![Fmt::Html], &c, t);
        assert_eq!(h.want(Want::Frame(7), Fmt::Html), vec![Act::FetchFrame(1, Fmt::Html)]);
        let acts = h.frame_announce(vec![Fmt::Text], &c, t);
        assert_eq!(acts[0], Act::Give { to: Want::Frame(7), fmt: Fmt::Html, data: None, src: None, copy: 1 });
        assert_eq!(h.frame_data(1, Fmt::Html, text("<b>")), vec![]);
        assert_eq!(h.copy(), 2);
    }

    #[test]
    fn a_remote_copy_is_fetched_lazily_from_its_channel() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        h.remote_announce(2, "laptop", vec![Fmt::Text, Fmt::Html], &c, t);
        h.remote_data(2, text("hi"), &c, t);
        assert_eq!(h.want(Want::Frame(1), Fmt::Text), vec![Act::Give { to: Want::Frame(1), fmt: Fmt::Text, data: arc("hi"), src: Some(2), copy: 1 }]);
        assert_eq!(h.want(Want::Rdp(0), Fmt::Html), vec![Act::FetchRdp(2, Fmt::Html)]);
        assert_eq!(h.remote_data(2, text("<i>x</i>"), &c, t), vec![Act::Give { to: Want::Rdp(0), fmt: Fmt::Html, data: arc("<i>x</i>"), src: Some(2), copy: 1 }]);
    }

    #[test]
    fn files_only_go_to_channels_that_take_them_and_echo_by_time() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        // a file copy on the desktop: no text to compare, so it's taken right away
        let acts = h.remote_announce(0, "desktop", vec![Fmt::Files], &c, t);
        assert_eq!(acts, vec![Act::OfferFrame(vec![Fmt::Files])]); // the laptop can't take files
        assert_eq!(h.remote_announce(1, "desktop", vec![Fmt::Files], &c, t), vec![]); // its other monitor
        // the Frame pastes: the descriptor comes from the channel it was copied on, as does the contents
        assert_eq!(h.want(Want::Frame(3), Fmt::Files), vec![Act::FetchRdp(0, Fmt::Files)]);
        let acts = h.remote_data(0, text("DESC"), &c, t);
        assert_eq!(acts, vec![Act::Give { to: Want::Frame(3), fmt: Fmt::Files, data: arc("DESC"), src: Some(0), copy: 1 }]);
        // a Frame file copy goes to the desktop; its other monitor echoing it inside the window is dropped
        h.frame_announce(vec![Fmt::Files], &c, t);
        assert_eq!(h.remote_announce(1, "desktop", vec![Fmt::Files], &c, t + Duration::from_secs(1)), vec![]);
        // after it, the same formats are a new copy
        let later = t + ECHO_TTL + Duration::from_secs(1);
        assert_eq!(h.remote_announce(1, "desktop", vec![Fmt::Files], &c, later), vec![Act::OfferFrame(vec![Fmt::Files])]);
    }

    #[test]
    fn an_empty_text_copy_with_nothing_else_is_ignored() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        h.remote_announce(0, "desktop", vec![Fmt::Text], &c, t);
        assert_eq!(h.remote_data(0, Some(vec![]), &c, t), vec![]);
        assert_eq!(h.owner(), None);
        // but empty text next to an image keeps the image
        h.remote_announce(0, "desktop", vec![Fmt::Text, Fmt::Image], &c, t);
        assert_eq!(h.remote_data(0, None, &c, t), vec![Act::OfferFrame(vec![Fmt::Image]), Act::Offer(2, vec![Fmt::Image])]);
    }

    #[test]
    fn a_channel_coming_up_gets_the_content_once_per_machine() {
        let (mut h, t) = (Hub::new(), Instant::now());
        assert_eq!(h.channel_up(&chans()[0], t), vec![]); // nothing copied yet
        h.frame_announce(vec![Fmt::Text], &[], t);
        assert_eq!(h.channel_up(&chans()[0], t), vec![Act::Offer(0, vec![Fmt::Text])]);
        assert_eq!(h.channel_up(&chans()[1], t), vec![]); // same machine
        h.remote_announce(2, "laptop", vec![Fmt::Text], &chans(), t);
        h.remote_data(2, text("x"), &chans(), t);
        assert_eq!(h.channel_up(&chans()[2], t), vec![]); // where it came from
    }

    #[test]
    fn a_channel_going_down_fails_what_it_owed_and_fetches_move_to_its_other_monitor() {
        let (mut h, c, t) = (Hub::new(), chans(), Instant::now());
        h.remote_announce(0, "desktop", vec![Fmt::Text, Fmt::Html], &c, t);
        h.remote_data(0, text("hi"), &c, t);
        assert_eq!(h.want(Want::Frame(1), Fmt::Html), vec![Act::FetchRdp(0, Fmt::Html)]);
        let acts = h.channel_down(0, &c);
        assert_eq!(acts, vec![Act::Give { to: Want::Frame(1), fmt: Fmt::Html, data: None, src: Some(1), copy: 1 }]);
        assert_eq!(h.want(Want::Frame(2), Fmt::Html), vec![Act::FetchRdp(1, Fmt::Html)]);
    }
}
