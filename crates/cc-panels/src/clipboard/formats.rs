//! What each clipboard format looks like on each side, and the conversions between them. The
//! hub keeps everything in the Frame session's form (UTF-8 text, an HTML fragment, a BMP file,
//! a text/uri-list) and these turn it into RDP's and back.
use super::Fmt;
use std::path::{Path, PathBuf};

pub const CF_TEXT: u32 = 1;
pub const CF_DIB: u32 = 8;
pub const CF_OEMTEXT: u32 = 7;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_DIBV5: u32 = 17;
pub const HTML_NAME: &str = "HTML Format";
pub const FILES_NAME: &str = "FileGroupDescriptorW";
// Registered formats get ids in 0xC000-0xFFFF from whoever announces them. These are the ones
// we announce ours with, and the other side asks for them by these ids.
pub const OUR_HTML: u32 = 0xC0C0;
pub const OUR_FILES: u32 = 0xC0C1;

/// The Frame session's MIME types for each format, the first one being what we ask for.
pub fn mimes(f: Fmt) -> &'static [&'static str] {
    match f {
        Fmt::Text => &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"],
        Fmt::Html => &["text/html"],
        Fmt::Image => &["image/bmp"],
        Fmt::Files => &["text/uri-list"],
    }
}

pub fn from_mime(m: &str) -> Option<Fmt> {
    [Fmt::Text, Fmt::Html, Fmt::Image, Fmt::Files].into_iter().find(|&f| mimes(f).contains(&m))
}

/// What a Frame offer's MIME types hold, and for each format the MIME type to ask for.
pub fn from_mimes<'a>(offered: &'a [String]) -> Vec<(Fmt, &'a str)> {
    let mut out: Vec<(Fmt, &str)> = Vec::new();
    for f in [Fmt::Text, Fmt::Html, Fmt::Image, Fmt::Files] {
        // the format's own order, so text/plain;charset=utf-8 wins over STRING
        if let Some(m) = mimes(f).iter().find_map(|m| offered.iter().find(|o| o.as_str() == *m)) {
            out.push((f, m));
        }
    }
    out
}

/// What an RDP format (its id, and its name for a registered one) is to us.
pub fn from_rdp(id: u32, name: Option<&str>) -> Option<Fmt> {
    match (id, name) {
        (CF_UNICODETEXT | CF_TEXT | CF_OEMTEXT, _) => Some(Fmt::Text),
        (CF_DIB | CF_DIBV5, _) => Some(Fmt::Image),
        (_, Some(HTML_NAME)) => Some(Fmt::Html),
        (_, Some(FILES_NAME)) => Some(Fmt::Files),
        _ => None,
    }
}

/// The id and name we announce a format with.
pub fn to_rdp(f: Fmt) -> (u32, Option<&'static str>) {
    match f {
        Fmt::Text => (CF_UNICODETEXT, None),
        Fmt::Html => (OUR_HTML, Some(HTML_NAME)),
        Fmt::Image => (CF_DIB, None),
        Fmt::Files => (OUR_FILES, Some(FILES_NAME)),
    }
}

/// From a server's format list, the id to ask for each format we can use. For text that's
/// always CF_UNICODETEXT (Windows makes it from the others), for images CF_DIB over CF_DIBV5.
pub fn pick(list: &[(u32, Option<String>)]) -> Vec<(Fmt, u32)> {
    let mut out: Vec<(Fmt, u32)> = Vec::new();
    for (id, name) in list {
        let Some(f) = from_rdp(*id, name.as_deref()) else { continue };
        let id = if f == Fmt::Text { CF_UNICODETEXT } else { *id };
        match out.iter_mut().find(|(g, _)| *g == f) {
            Some(e) if f == Fmt::Image && id == CF_DIB => e.1 = id,
            Some(_) => {}
            None => out.push((f, id)),
        }
    }
    out
}

// ------------------------------------------------------------------ text

/// CF_UNICODETEXT (UTF-16LE, NUL-terminated) to a string, up to its first NUL.
pub fn from_utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

pub fn to_utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

// ------------------------------------------------------------------ HTML

/// An HTML fragment as CF_HTML: a header of byte offsets, then the page with the fragment marked.
pub fn to_cf_html(fragment: &str) -> Vec<u8> {
    const HEAD: &str = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let pre = "<html><body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body></html>";
    let start = HEAD.len();
    let frag = start + pre.len();
    let end_frag = frag + fragment.len();
    let end = end_frag + post.len();
    let head = format!("Version:0.9\r\nStartHTML:{start:010}\r\nEndHTML:{end:010}\r\nStartFragment:{frag:010}\r\nEndFragment:{end_frag:010}\r\n");
    [head.as_bytes(), pre.as_bytes(), fragment.as_bytes(), post.as_bytes()].concat()
}

/// CF_HTML's fragment (or its whole page, if it marks no fragment).
pub fn from_cf_html(b: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(b);
    let field = |k: &str| -> Option<usize> {
        let at = text.find(&format!("{k}:"))? + k.len() + 1;
        text[at..].split(['\r', '\n']).next()?.trim().parse().ok()
    };
    let (s, e) = match (field("StartFragment"), field("EndFragment")) {
        (Some(s), Some(e)) => (s, e),
        _ => (field("StartHTML")?, field("EndHTML")?),
    };
    let e = e.min(b.len());
    (s < e).then(|| String::from_utf8_lossy(&b[s..e]).into_owned())
}

// ------------------------------------------------------------------ images

/// CF_DIB (a BITMAPINFO and its pixels) as a BMP file: the same with a 14-byte file header.
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |i: usize| dib.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let size = u32_at(0)? as usize; // the header's own size: 40 for BITMAPINFOHEADER, 124 for V5
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().unwrap());
    let compression = u32_at(16)?;
    let used = u32_at(32)? as usize;
    let colours = if used != 0 { used } else if bits <= 8 { 1 << bits } else { 0 };
    let masks = if size == 40 && compression == 3 { 12 } else { 0 }; // BI_BITFIELDS after a plain header
    let off = 14 + size + masks + colours * 4;
    if size < 40 || off - 14 > dib.len() {
        return None;
    }
    let mut out = Vec::with_capacity(14 + dib.len());
    out.extend(b"BM");
    out.extend(((14 + dib.len()) as u32).to_le_bytes());
    out.extend([0; 4]);
    out.extend((off as u32).to_le_bytes());
    out.extend(dib);
    Some(out)
}

pub fn bmp_to_dib(bmp: &[u8]) -> Option<Vec<u8>> {
    (bmp.len() > 14 + 40 && bmp.starts_with(b"BM")).then(|| bmp[14..].to_vec())
}

// ------------------------------------------------------------------ files

pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FD_ATTRIBUTES: u32 = 0x4;
const FD_FILESIZE: u32 = 0x40;
const FD_WRITESTIME: u32 = 0x20;
const FD_SHOWPROGRESSUI: u32 = 0x4000;
const DESCRIPTOR: usize = 592; // FILEDESCRIPTORW

/// One entry of a FileGroupDescriptorW: its path relative to what was copied (with \ between
/// folders, as RDP has it), its size, whether it's a folder, and, for ours, where it is here.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub dir: bool,
    pub local: Option<PathBuf>,
}

/// CLIPRDR_FILELIST: a count, then a FILEDESCRIPTORW each.
pub fn to_descriptor(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + entries.len() * DESCRIPTOR);
    out.extend((entries.len() as u32).to_le_bytes());
    for e in entries {
        let mut d = [0u8; DESCRIPTOR];
        d[0..4].copy_from_slice(&(FD_ATTRIBUTES | FD_FILESIZE | FD_WRITESTIME | FD_SHOWPROGRESSUI).to_le_bytes());
        let attr = if e.dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
        d[36..40].copy_from_slice(&attr.to_le_bytes());
        let modified = e.local.as_ref().and_then(|p| std::fs::metadata(p).ok()?.modified().ok());
        let secs = modified.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        d[56..64].copy_from_slice(&((secs + 11_644_473_600) * 10_000_000).to_le_bytes()); // FILETIME: 100 ns since 1601
        d[64..68].copy_from_slice(&((e.size >> 32) as u32).to_le_bytes());
        d[68..72].copy_from_slice(&(e.size as u32).to_le_bytes());
        for (i, u) in e.name.encode_utf16().take(259).enumerate() {
            d[72 + i * 2..74 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        out.extend(d);
    }
    out
}

pub fn from_descriptor(b: &[u8]) -> Option<Vec<Entry>> {
    let n = u32::from_le_bytes(b.get(0..4)?.try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(n.min(10_000));
    for i in 0..n {
        let d = b.get(4 + i * DESCRIPTOR..4 + (i + 1) * DESCRIPTOR)?;
        let u = |a: usize| u32::from_le_bytes(d[a..a + 4].try_into().unwrap());
        let dir = u(36) & FILE_ATTRIBUTE_DIRECTORY != 0;
        let size = (u(64) as u64) << 32 | u(68) as u64;
        out.push(Entry { name: from_utf16(&d[72..]), size, dir, local: None });
    }
    Some(out)
}

/// The paths in a text/uri-list (file: URIs only).
pub fn from_uri_list(b: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(b).lines().map(str::trim).filter(|l| !l.starts_with('#')).filter_map(|l| {
        let rest = l.strip_prefix("file://")?;
        let path = &rest[rest.find('/')?..]; // past a host name, if there's one
        Some(PathBuf::from(unescape(path)))
    }).collect()
}

pub fn to_uri_list(paths: &[PathBuf]) -> Vec<u8> {
    paths.iter().map(|p| format!("file://{}\r\n", escape(&p.to_string_lossy()))).collect::<String>().into_bytes()
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%').then(|| b.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match hex {
            Some(v) => (out.push(v), i += 3),
            None => (out.push(b[i]), i += 1),
        };
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape(s: &str) -> String {
    s.bytes().map(|c| if c.is_ascii_alphanumeric() || b"/-_.~".contains(&c) { (c as char).to_string() } else { format!("%{c:02X}") }).collect()
}

/// What was copied in the Frame session as descriptor entries: each file, and each folder
/// followed by everything in it, named relative to the copied thing's parent.
pub fn walk(paths: &[PathBuf]) -> Vec<Entry> {
    fn add(out: &mut Vec<Entry>, path: &Path, name: String) {
        let Ok(m) = std::fs::symlink_metadata(path) else { return };
        if m.is_dir() {
            out.push(Entry { name: name.clone(), size: 0, dir: true, local: Some(path.into()) });
            let mut kids: Vec<_> = std::fs::read_dir(path).into_iter().flatten().flatten().collect();
            kids.sort_by_key(|k| k.file_name());
            for k in kids {
                add(out, &k.path(), format!("{name}\\{}", k.file_name().to_string_lossy()));
            }
        } else if m.is_file() {
            out.push(Entry { name, size: m.len(), dir: false, local: Some(path.into()) });
        } // ponytail: symlinks and sockets get skipped, not followed
    }
    let mut out = Vec::new();
    for p in paths {
        if let Some(n) = p.file_name() {
            add(&mut out, p, n.to_string_lossy().into_owned());
        }
    }
    out
}

/// Where a descriptor entry lands under the staging folder. A name that climbs out of it
/// (`..`, an absolute path) is refused, since the other machine chose it.
pub fn staged(root: &Path, name: &str) -> Option<PathBuf> {
    let mut p = root.to_path_buf();
    for part in name.split(['\\', '/']) {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            return None;
        }
        p.push(part);
    }
    Some(p)
}

/// The top-level things in a staged copy, for the uri-list.
pub fn tops(root: &Path, entries: &[Entry]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for e in entries {
        let top = e.name.split(['\\', '/']).next().unwrap_or_default();
        let p = root.join(top);
        if !top.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips_as_utf16() {
        let b = to_utf16("héllo ✓");
        assert!(b.ends_with(&[0, 0]));
        assert_eq!(from_utf16(&b), "héllo ✓");
        assert_eq!(from_utf16(&[0, 0]), ""); // krdp's empty copy (an image on its side)
        assert_eq!(from_utf16(&[b'a', 0, 0, 0, b'b', 0]), "a"); // stops at the NUL
    }

    #[test]
    fn mimes_and_rdp_ids_map_both_ways() {
        let offered: Vec<String> = ["TEXT", "text/plain;charset=utf-8", "text/html", "x-kde-junk", "text/uri-list"].map(String::from).into();
        assert_eq!(from_mimes(&offered), vec![(Fmt::Text, "text/plain;charset=utf-8"), (Fmt::Html, "text/html"), (Fmt::Files, "text/uri-list")]);
        assert_eq!(from_mime("UTF8_STRING"), Some(Fmt::Text));
        assert_eq!(from_mime("image/png"), None);
        let list = vec![(CF_TEXT, None), (CF_DIBV5, None), (CF_DIB, None), (0xC123, Some("HTML Format".into())), (0xC124, Some("FileGroupDescriptorW".into())), (0xC125, Some("Rich Text Format".into()))];
        assert_eq!(pick(&list), vec![(Fmt::Text, CF_UNICODETEXT), (Fmt::Image, CF_DIB), (Fmt::Html, 0xC123), (Fmt::Files, 0xC124)]);
        for f in [Fmt::Text, Fmt::Html, Fmt::Image, Fmt::Files] {
            let (id, name) = to_rdp(f);
            assert_eq!(from_rdp(id, name), Some(f));
        }
    }

    #[test]
    fn cf_html_round_trips() {
        let b = to_cf_html("<b>hi</b> ✓");
        let s = String::from_utf8(b.clone()).unwrap();
        let at = |k: &str| s[s.find(k).unwrap() + k.len() + 1..][..10].parse::<usize>().unwrap();
        assert_eq!(&b[at("StartFragment")..at("EndFragment")], "<b>hi</b> ✓".as_bytes());
        assert_eq!(&s[at("StartHTML")..at("StartHTML") + 6], "<html>");
        assert_eq!(from_cf_html(&b).as_deref(), Some("<b>hi</b> ✓"));
        assert_eq!(from_cf_html(b"no header"), None);
    }

    #[test]
    fn dib_becomes_bmp_and_back() {
        // 1x1, 24-bit, BITMAPINFOHEADER: 40 bytes, then 4 bytes of pixel row
        let mut dib = vec![0u8; 44];
        dib[0] = 40;
        dib[4] = 1;
        dib[8] = 1;
        dib[12] = 1;
        dib[14] = 24;
        let bmp = dib_to_bmp(&dib).unwrap();
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 58);
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54); // pixels right after the header
        assert_eq!(bmp_to_dib(&bmp).unwrap(), dib);
        dib[14] = 8; // 8-bit: a 256-colour palette comes first
        assert_eq!(dib_to_bmp(&dib), None); // and this one's too short to have it
        assert_eq!(dib_to_bmp(&[1, 2]), None);
    }

    #[test]
    fn descriptors_round_trip_and_staging_stays_inside() {
        let e = vec![
            Entry { name: "photos".into(), size: 0, dir: true, local: None },
            Entry { name: "photos\\a b.jpg".into(), size: 5_000_000_000, dir: false, local: None },
        ];
        let b = to_descriptor(&e);
        assert_eq!(b.len(), 4 + 2 * 592);
        assert_eq!(from_descriptor(&b).unwrap(), e);
        assert_eq!(from_descriptor(&b[..600]), None); // says 2, has 1
        let root = Path::new("/tmp/stage");
        assert_eq!(staged(root, "photos\\a b.jpg"), Some(PathBuf::from("/tmp/stage/photos/a b.jpg")));
        assert_eq!(staged(root, "..\\..\\.bashrc"), None);
        assert_eq!(staged(root, "C:\\Windows"), None);
        assert_eq!(staged(root, "\\etc\\passwd"), None);
        assert_eq!(tops(root, &e), vec![PathBuf::from("/tmp/stage/photos")]);
    }

    #[test]
    fn uri_lists_round_trip() {
        let p = vec![PathBuf::from("/home/me/a b%.txt"), PathBuf::from("/tmp/ü")];
        let l = to_uri_list(&p);
        assert_eq!(String::from_utf8_lossy(&l), "file:///home/me/a%20b%25.txt\r\nfile:///tmp/%C3%BC\r\n");
        assert_eq!(from_uri_list(&l), p);
        assert_eq!(from_uri_list(b"# comment\nfile://host/x\nhttps://example.com\n"), vec![PathBuf::from("/x")]);
    }

    #[test]
    fn walking_a_folder_lists_it_then_its_files() {
        let d = std::env::temp_dir().join(format!("cc-clip-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("top/sub")).unwrap();
        std::fs::write(d.join("top/sub/f.txt"), b"12345").unwrap();
        std::fs::write(d.join("one.txt"), b"1").unwrap();
        let e = walk(&[d.join("top"), d.join("one.txt")]);
        let names: Vec<_> = e.iter().map(|e| (e.name.as_str(), e.size, e.dir)).collect();
        assert_eq!(names, vec![("top", 0, true), ("top\\sub", 0, true), ("top\\sub\\f.txt", 5, false), ("one.txt", 1, false)]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
