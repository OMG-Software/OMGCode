//! A small, in-house HTML to text converter.
//!
//! The page is hostile input that ends up in a model's context, so the
//! converter is a single forward pass with no recursion and no dependency.
//! Every step moves the read position forward, so the work is linear in the
//! input, and the output and the open-element stack are bounded.
//!
//! It is not an HTML5 parser and does not claim browser fidelity. It drops
//! what a reader would not see or what carries no text: scripts, styles,
//! comments, the head (except the title), and any element a page hides with a
//! `hidden` attribute, `aria-hidden="true"` or an inline `display:none` or
//! `visibility:hidden`. Hidden text is a common carrier for prompt injection.
//! It cannot see CSS from style sheets or classes, so a page can still hide
//! text from this converter that a browser would hide; the output is
//! untrusted text either way and grants no authority.
//!
//! An element that is never closed stays open to the end of the input, so a
//! hidden element without an end tag drops the rest of the page. That fails
//! closed. More than [`MAX_OPEN_ELEMENTS`] open elements stops the
//! conversion and reports truncation.

use reqwest::Url;

/// The most open elements tracked at once.
pub(super) const MAX_OPEN_ELEMENTS: usize = 512;
/// The longest link target written, in bytes of the resolved URL.
const MAX_LINK_BYTES: usize = 512;
/// The longest title kept, in bytes.
const MAX_TITLE_BYTES: usize = 512;
/// The longest attribute value inspected. A longer `style` counts as hidden.
const MAX_ATTRIBUTE_BYTES: usize = 8 * 1024;
/// The longest tag name that is classified; a longer name is `Other`.
const MAX_TAG_NAME_BYTES: usize = 32;
/// Consecutive newlines kept inside preformatted text.
const MAX_PRE_NEWLINES: u8 = 3;

/// The converted text.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct HtmlText {
    /// The plain text.
    pub(super) text: String,
    /// Whether the output cap or the open-element cap cut the conversion.
    pub(super) truncated: bool,
}

/// Convert `html` to plain text of at most `max_bytes` bytes.
///
/// `base` is the page URL against which link targets are resolved. The
/// returned text may still hold control and bidirectional characters; the
/// caller removes them.
pub(super) fn html_to_text(html: &str, base: &Url, max_bytes: usize) -> HtmlText {
    // The title is written first and is added to the budget up front.
    let reserve = (MAX_TITLE_BYTES + 2).min(max_bytes / 4);
    let mut converter = Converter::new(base, max_bytes.saturating_sub(reserve), max_bytes);
    converter.run(html);
    converter.finish()
}

/// The element classes the converter acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Html,
    Head,
    Body,
    Title,
    Script,
    Style,
    /// noscript, template, svg, math, iframe, object: content dropped.
    Skip,
    P,
    Heading(u8),
    Li,
    /// dt and dd.
    DefinitionItem,
    Pre,
    Code,
    Anchor,
    Table,
    Row,
    Cell,
    Br,
    Hr,
    /// Void elements with no effect on the text.
    Void,
    /// Other block-level elements: one line break around the content.
    Block,
    Other,
}

impl Kind {
    fn from_name(name: &str) -> Self {
        match name {
            "html" => Self::Html,
            "head" => Self::Head,
            "body" => Self::Body,
            "title" => Self::Title,
            "script" => Self::Script,
            "style" => Self::Style,
            "noscript" | "template" | "svg" | "math" | "iframe" | "object" => Self::Skip,
            "p" => Self::P,
            "h1" => Self::Heading(1),
            "h2" => Self::Heading(2),
            "h3" => Self::Heading(3),
            "h4" => Self::Heading(4),
            "h5" => Self::Heading(5),
            "h6" => Self::Heading(6),
            "li" => Self::Li,
            "dt" | "dd" => Self::DefinitionItem,
            "pre" => Self::Pre,
            "code" => Self::Code,
            "a" => Self::Anchor,
            "table" => Self::Table,
            "tr" => Self::Row,
            "td" | "th" => Self::Cell,
            "br" => Self::Br,
            "hr" => Self::Hr,
            "area" | "base" | "col" | "embed" | "img" | "input" | "link" | "meta" | "param"
            | "source" | "track" | "wbr" | "keygen" | "basefont" | "frame" => Self::Void,
            "address" | "article" | "aside" | "blockquote" | "details" | "dialog" | "div"
            | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "header"
            | "hgroup" | "main" | "nav" | "ol" | "section" | "summary" | "ul" | "thead"
            | "tbody" | "tfoot" | "caption" | "menu" | "center" => Self::Block,
            _ => Self::Other,
        }
    }

    /// Elements with no end tag and no content.
    fn is_void(self) -> bool {
        matches!(self, Self::Void | Self::Br | Self::Hr)
    }

    /// Whether a start tag of this kind ends an open head.
    fn ends_head(self) -> bool {
        !matches!(
            self,
            Self::Head
                | Self::Html
                | Self::Title
                | Self::Script
                | Self::Style
                | Self::Skip
                | Self::Void
        )
    }

    /// Whether a start tag of this kind ends an open `p`.
    fn ends_paragraph(self) -> bool {
        matches!(
            self,
            Self::P
                | Self::Heading(_)
                | Self::Block
                | Self::Pre
                | Self::Table
                | Self::Li
                | Self::DefinitionItem
        )
    }
}

/// One open element.
#[derive(Clone, Copy, Debug)]
struct Entry {
    kind: Kind,
    /// FNV-1a of the lower-case tag name, to match end tags.
    hash: u64,
    /// The content is dropped, including a head.
    skips: bool,
    /// The content is dropped for a reason other than a head.
    strong: bool,
    /// The element's start was written, so its end is written too.
    emitted: bool,
    /// This element opened the open link.
    link: bool,
    /// This element wrote an opening backtick.
    tick: bool,
}

/// The output buffer and its whitespace state.
#[derive(Debug)]
struct Out {
    buf: String,
    cap: usize,
    truncated: bool,
    pending_space: bool,
    pending_breaks: u8,
    /// Start and end of the last list or heading prefix.
    prefix: Option<(usize, usize)>,
}

impl Out {
    fn new(cap: usize) -> Self {
        Self {
            buf: String::new(),
            cap,
            truncated: false,
            pending_space: false,
            pending_breaks: 0,
            prefix: None,
        }
    }

    fn push_str(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        if self
            .buf
            .len()
            .checked_add(text.len())
            .is_none_or(|total| total > self.cap)
        {
            self.truncated = true;
            return;
        }
        self.buf.push_str(text);
    }

    fn push_char(&mut self, c: char) {
        let mut encoded = [0_u8; 4];
        self.push_str(c.encode_utf8(&mut encoded));
    }

    fn trailing_newlines(&self) -> u8 {
        let mut count = 0;
        for byte in self.buf.bytes().rev().take(usize::from(MAX_PRE_NEWLINES)) {
            if byte != b'\n' {
                break;
            }
            count += 1;
        }
        count
    }

    /// Write the line breaks or space that are waiting.
    fn flush(&mut self) {
        if self.buf.is_empty() {
            self.pending_breaks = 0;
            self.pending_space = false;
            return;
        }
        if self.pending_breaks > 0 {
            while self.buf.ends_with(' ') {
                self.buf.pop();
            }
            for _ in self.trailing_newlines()..self.pending_breaks {
                self.push_char('\n');
            }
        } else if self.pending_space && !self.buf.ends_with([' ', '\n']) {
            self.push_char(' ');
        }
        self.pending_breaks = 0;
        self.pending_space = false;
    }

    /// Request `count` line breaks before the next text. Directly after a
    /// list or heading prefix the request is ignored, so `<li><p>` stays on
    /// one line.
    fn breaks(&mut self, count: u8) {
        if self.after_prefix() {
            return;
        }
        self.pending_breaks = self.pending_breaks.max(count);
        self.pending_space = false;
    }

    fn line_break(&mut self) {
        if self.after_prefix() {
            return;
        }
        self.pending_breaks = if self.pending_breaks >= 1 { 2 } else { 1 };
        self.pending_space = false;
    }

    fn after_prefix(&self) -> bool {
        self.prefix.is_some_and(|(_, end)| end == self.buf.len())
    }

    fn word(&mut self, c: char) {
        self.flush();
        self.push_char(c);
    }

    /// One character of collapsed text.
    fn collapsed(&mut self, c: char) {
        if c.is_whitespace() {
            self.pending_space = true;
        } else {
            self.word(c);
        }
    }

    /// One character of preformatted text.
    fn preformatted(&mut self, c: char) {
        match c {
            '\r' => {}
            '\n' => {
                self.flush();
                if self.trailing_newlines() < MAX_PRE_NEWLINES {
                    self.push_char('\n');
                }
            }
            _ => self.word(c),
        }
    }

    /// Write text after any waiting break, such as an opening backtick.
    fn open_inline(&mut self, text: &str) -> usize {
        self.flush();
        let start = self.buf.len();
        self.push_str(text);
        start
    }

    /// Write text with no space before it, such as a table cell separator.
    fn tight(&mut self, text: &str) {
        self.pending_space = false;
        self.flush();
        self.push_str(text);
    }

    /// Write a closing marker, such as a backtick or `](`, directly after the
    /// text it closes. A waiting break or space is kept for the text after it.
    fn close_inline(&mut self, text: &str) {
        let breaks = std::mem::take(&mut self.pending_breaks);
        let space = std::mem::take(&mut self.pending_space);
        self.push_str(text);
        self.pending_breaks = breaks;
        self.pending_space = space;
    }

    /// Write a list or heading prefix, remembering it so an empty one can go.
    fn prefix(&mut self, text: &str) {
        let start = self.open_inline(text);
        self.prefix = Some((start, self.buf.len()));
    }

    fn drop_empty_prefix(&mut self) {
        if let Some((start, end)) = self.prefix.take()
            && end == self.buf.len()
            && !self.truncated
        {
            self.buf.truncate(start);
        }
    }

    fn close_fence(&mut self) {
        self.pending_space = false;
        self.pending_breaks = 0;
        if !self.buf.ends_with('\n') {
            self.push_char('\n');
        }
        self.push_str("```");
    }
}

/// An open link: the text so far starts at `start`, after its `[`.
#[derive(Debug)]
struct LinkState {
    url: String,
    start: usize,
}

struct Converter<'a> {
    base: &'a Url,
    out: Out,
    stack: Vec<Entry>,
    skip_depth: usize,
    strong_skip_depth: usize,
    head_depth: usize,
    pre_depth: usize,
    pre_fresh: bool,
    row_has_cell: bool,
    title: String,
    link: Option<LinkState>,
    stopped: bool,
    /// The cap on the whole text, title included.
    max_bytes: usize,
}

/// What a start tag's attributes say.
#[derive(Debug, Default)]
struct Attributes {
    hidden: bool,
    href: Option<String>,
}

impl<'a> Converter<'a> {
    fn new(base: &'a Url, cap: usize, max_bytes: usize) -> Self {
        Self {
            base,
            max_bytes,
            out: Out::new(cap),
            stack: Vec::new(),
            skip_depth: 0,
            strong_skip_depth: 0,
            head_depth: 0,
            pre_depth: 0,
            pre_fresh: false,
            row_has_cell: false,
            title: String::new(),
            link: None,
            stopped: false,
        }
    }

    fn done(&self) -> bool {
        self.stopped || self.out.truncated
    }

    fn run(&mut self, html: &str) {
        let bytes = html.as_bytes();
        let mut at = 0;
        while at < bytes.len() && !self.done() {
            let Some(relative) = bytes
                .get(at..)
                .and_then(|rest| rest.iter().position(|&byte| byte == b'<'))
            else {
                self.text(html.get(at..).unwrap_or(""));
                break;
            };
            let lt = at + relative;
            if lt > at {
                self.text(html.get(at..lt).unwrap_or(""));
            }
            at = self.markup(html, bytes, lt);
        }
    }

    fn finish(mut self) -> HtmlText {
        while !self.stack.is_empty() {
            self.pop();
        }
        self.close_link();
        let mut body = self.out.buf;
        body.truncate(body.trim_end().len());
        let mut truncated = self.out.truncated;
        let mut text = if self.title.is_empty() {
            body
        } else if body.is_empty() {
            self.title
        } else {
            format!("{}\n\n{body}", self.title)
        };
        // The title reserve is only a share of a small cap, so a long title
        // can still push the whole past it; cut on a character boundary.
        if text.len() > self.max_bytes {
            let mut end = self.max_bytes;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
        HtmlText { text, truncated }
    }

    /// Handle the construct at `lt` and return where to read next, which is
    /// always after `lt`.
    fn markup(&mut self, html: &str, bytes: &[u8], lt: usize) -> usize {
        let after = lt + 1;
        match bytes.get(after) {
            Some(b'!') => {
                if bytes
                    .get(lt..)
                    .is_some_and(|rest| rest.starts_with(b"<!--"))
                {
                    end_after(bytes, lt + 2, b"-->")
                } else if bytes
                    .get(lt..)
                    .is_some_and(|rest| rest.starts_with(b"<![CDATA["))
                {
                    end_after(bytes, lt + 9, b"]]>")
                } else {
                    end_after_byte(bytes, lt + 2, b'>')
                }
            }
            Some(b'?') => end_after_byte(bytes, lt + 2, b'>'),
            Some(b'/') => {
                if bytes.get(lt + 2).is_some_and(u8::is_ascii_alphabetic) {
                    let (hash, end) = scan_name(bytes, lt + 2);
                    self.end_tag(hash);
                    end_after_byte(bytes, end, b'>')
                } else {
                    end_after_byte(bytes, lt + 2, b'>')
                }
            }
            Some(byte) if byte.is_ascii_alphabetic() => self.start_tag(html, bytes, after),
            _ => {
                self.text("<");
                after
            }
        }
    }

    fn start_tag(&mut self, html: &str, bytes: &[u8], name_start: usize) -> usize {
        let (hash, name_end) = scan_name(bytes, name_start);
        let name = html
            .get(name_start..name_end)
            .filter(|name| name.len() <= MAX_TAG_NAME_BYTES)
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let kind = Kind::from_name(&name);
        let Some((attributes, end)) = scan_attributes(bytes, name_end) else {
            // An unterminated tag swallows the rest of the page.
            return bytes.len();
        };
        match kind {
            Kind::Title => {
                let close = find_ci(bytes, end, b"</title").unwrap_or(bytes.len());
                if self.strong_skip_depth == 0 && self.title.is_empty() {
                    self.capture_title(html.get(end..close).unwrap_or(""));
                }
                close
            }
            Kind::Script | Kind::Style => {
                self.open(kind, hash, &attributes);
                let needle: &[u8] = if kind == Kind::Script {
                    b"</script"
                } else {
                    b"</style"
                };
                find_ci(bytes, end, needle).unwrap_or(bytes.len())
            }
            _ => {
                self.open(kind, hash, &attributes);
                end
            }
        }
    }

    fn capture_title(&mut self, raw: &str) {
        let mut decoded = String::new();
        decode_entities(raw, |c| decoded.push(c));
        let mut title = String::new();
        for word in decoded.split_whitespace() {
            if !title.is_empty() {
                title.push(' ');
            }
            title.push_str(word);
            if title.len() > MAX_TITLE_BYTES {
                break;
            }
        }
        let mut end = title.len().min(MAX_TITLE_BYTES);
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        title.truncate(end);
        self.title = title;
    }

    fn open(&mut self, kind: Kind, hash: u64, attributes: &Attributes) {
        if self.head_depth > 0 && kind.ends_head() {
            while self.head_depth > 0 {
                self.pop();
            }
        }
        self.close_implied(kind);
        if kind.is_void() {
            self.void_element(kind);
            return;
        }
        if self.stack.len() >= MAX_OPEN_ELEMENTS {
            self.stopped = true;
            self.out.truncated = true;
            return;
        }
        let strong = attributes.hidden || matches!(kind, Kind::Script | Kind::Style | Kind::Skip);
        let skips = strong || kind == Kind::Head;
        let emitted = self.skip_depth == 0 && !skips;
        let mut entry = Entry {
            kind,
            hash,
            skips,
            strong,
            emitted,
            link: false,
            tick: false,
        };
        if emitted {
            self.start_effect(&mut entry, attributes);
        }
        if skips {
            self.skip_depth += 1;
        }
        if strong {
            self.strong_skip_depth += 1;
        }
        if kind == Kind::Head {
            self.head_depth += 1;
        }
        self.stack.push(entry);
    }

    /// End the elements that HTML closes without an end tag.
    fn close_implied(&mut self, kind: Kind) {
        let top = self.stack.last().map(|entry| entry.kind);
        match (kind, top) {
            (Kind::Li, Some(Kind::Li))
            | (Kind::DefinitionItem, Some(Kind::DefinitionItem))
            | (Kind::Cell, Some(Kind::Cell))
            | (Kind::Row, Some(Kind::Row)) => self.pop(),
            (Kind::Row, Some(Kind::Cell)) => {
                self.pop();
                if self.stack.last().map(|entry| entry.kind) == Some(Kind::Row) {
                    self.pop();
                }
            }
            (kind, Some(Kind::P)) if kind.ends_paragraph() => self.pop(),
            _ => {}
        }
    }

    fn void_element(&mut self, kind: Kind) {
        if self.skip_depth > 0 {
            return;
        }
        match kind {
            Kind::Br if self.pre_depth > 0 => self.out.preformatted('\n'),
            Kind::Br => self.out.line_break(),
            Kind::Hr => self.out.breaks(1),
            _ => {}
        }
    }

    fn start_effect(&mut self, entry: &mut Entry, attributes: &Attributes) {
        match entry.kind {
            Kind::P => self.out.breaks(2),
            Kind::Heading(level) => {
                self.out.breaks(2);
                let prefix = match level {
                    1 => "# ",
                    2 => "## ",
                    3 => "### ",
                    4 => "#### ",
                    5 => "##### ",
                    _ => "###### ",
                };
                self.out.prefix(prefix);
            }
            Kind::Li => {
                self.out.breaks(1);
                self.out.prefix("- ");
            }
            Kind::DefinitionItem | Kind::Block => self.out.breaks(1),
            Kind::Table => {
                self.out.breaks(2);
                self.row_has_cell = false;
            }
            Kind::Row => {
                self.out.breaks(1);
                self.row_has_cell = false;
            }
            Kind::Cell => {
                if self.row_has_cell {
                    self.out.tight(" | ");
                }
                self.row_has_cell = true;
            }
            Kind::Pre => {
                self.out.breaks(2);
                self.out.open_inline("```\n");
                self.pre_depth += 1;
                self.pre_fresh = true;
            }
            Kind::Code if self.pre_depth == 0 => {
                self.out.open_inline("`");
                entry.tick = true;
            }
            Kind::Anchor => {
                let target = attributes
                    .href
                    .as_deref()
                    .and_then(|href| link_target(self.base, href));
                if let Some(url) = target {
                    // A link cannot nest, so an open one ends here.
                    while self.link.is_some() && !self.stack.is_empty() {
                        self.pop();
                    }
                    let start = self.out.open_inline("[");
                    self.link = Some(LinkState {
                        url,
                        start: start + 1,
                    });
                    entry.link = true;
                }
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, hash: u64) {
        let Some(index) = self.stack.iter().rposition(|entry| entry.hash == hash) else {
            return;
        };
        while self.stack.len() > index {
            self.pop();
        }
    }

    fn pop(&mut self) {
        let Some(entry) = self.stack.pop() else {
            return;
        };
        if entry.skips {
            self.skip_depth = self.skip_depth.saturating_sub(1);
        }
        if entry.strong {
            self.strong_skip_depth = self.strong_skip_depth.saturating_sub(1);
        }
        if entry.kind == Kind::Head {
            self.head_depth = self.head_depth.saturating_sub(1);
        }
        if !entry.emitted {
            return;
        }
        match entry.kind {
            Kind::P | Kind::Table => self.out.breaks(2),
            Kind::Heading(_) => {
                self.out.drop_empty_prefix();
                self.out.breaks(2);
            }
            Kind::Li => {
                self.out.drop_empty_prefix();
                self.out.breaks(1);
            }
            Kind::DefinitionItem | Kind::Block | Kind::Row => self.out.breaks(1),
            Kind::Pre => {
                self.pre_depth = self.pre_depth.saturating_sub(1);
                self.pre_fresh = false;
                self.out.close_fence();
                self.out.breaks(2);
            }
            Kind::Code if entry.tick => self.out.close_inline("`"),
            Kind::Anchor if entry.link => self.close_link(),
            _ => {}
        }
    }

    fn close_link(&mut self) {
        let Some(link) = self.link.take() else {
            return;
        };
        if self.out.truncated {
            return;
        }
        if self.out.buf.len() <= link.start {
            // No text: remove the opening bracket too.
            self.out.buf.truncate(link.start.saturating_sub(1));
        } else {
            // A break or space that followed the text comes after the link.
            self.out.close_inline("](");
            self.out.push_str(&link.url);
            self.out.push_str(")");
        }
    }

    /// Write text between tags, decoding entities.
    fn text(&mut self, raw: &str) {
        if self.skip_depth > 0 || self.done() {
            return;
        }
        let preformatted = self.pre_depth > 0;
        let mut fresh = self.pre_fresh;
        decode_entities(raw, |c| {
            if preformatted {
                if fresh {
                    fresh = false;
                    if c == '\n' {
                        return;
                    }
                }
                self.out.preformatted(c);
            } else {
                self.out.collapsed(c);
            }
        });
        self.pre_fresh = fresh;
    }
}

/// Resolve a link target against the page, or `None` when it is not an
/// absolute http or https URL of at most [`MAX_LINK_BYTES`] bytes without
/// credentials. Parentheses are escaped so the target cannot end the link.
fn link_target(base: &Url, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let url = base.join(href).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.as_str().len() > MAX_LINK_BYTES
    {
        return None;
    }
    Some(url.as_str().replace('(', "%28").replace(')', "%29"))
}

/// Scan a tag name from `start`; returns its hash and the end index.
fn scan_name(bytes: &[u8], start: usize) -> (u64, usize) {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut at = start;
    while let Some(&byte) = bytes.get(at) {
        if byte.is_ascii_whitespace() || byte == b'/' || byte == b'>' {
            break;
        }
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        at += 1;
    }
    (hash, at)
}

/// Read the attributes of a start tag from `at`, after its name. Returns the
/// facts the converter uses and the index after the closing `>`, or `None`
/// when the tag never ends.
fn scan_attributes(bytes: &[u8], mut at: usize) -> Option<(Attributes, usize)> {
    let mut attributes = Attributes::default();
    loop {
        while bytes
            .get(at)
            .is_some_and(|&byte| byte.is_ascii_whitespace() || byte == b'/')
        {
            at += 1;
        }
        match bytes.get(at) {
            None => return None,
            Some(b'>') => return Some((attributes, at + 1)),
            Some(_) => {}
        }
        let name_start = at;
        while bytes.get(at).is_some_and(|&byte| {
            !(byte.is_ascii_whitespace() || matches!(byte, b'=' | b'>' | b'/'))
        }) {
            at += 1;
        }
        if at == name_start {
            // A stray `=`: skip it so the scan always advances.
            at += 1;
            continue;
        }
        let name = bytes.get(name_start..at).unwrap_or_default();
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let mut value: &[u8] = &[];
        if bytes.get(at) == Some(&b'=') {
            at += 1;
            while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            match bytes.get(at) {
                Some(&quote) if quote == b'"' || quote == b'\'' => {
                    let start = at + 1;
                    let length = bytes.get(start..)?.iter().position(|&byte| byte == quote)?;
                    value = bytes.get(start..start + length).unwrap_or_default();
                    at = start + length + 1;
                }
                _ => {
                    let start = at;
                    while bytes
                        .get(at)
                        .is_some_and(|&byte| !(byte.is_ascii_whitespace() || byte == b'>'))
                    {
                        at += 1;
                    }
                    value = bytes.get(start..at).unwrap_or_default();
                }
            }
        }
        note_attribute(&mut attributes, name, value);
    }
}

fn note_attribute(attributes: &mut Attributes, name: &[u8], value: &[u8]) {
    if name.eq_ignore_ascii_case(b"hidden") {
        attributes.hidden = true;
    } else if name.eq_ignore_ascii_case(b"aria-hidden") {
        if value.len() <= 64 && decode_attribute(value).trim().eq_ignore_ascii_case("true") {
            attributes.hidden = true;
        }
    } else if name.eq_ignore_ascii_case(b"style") {
        // A style too long to inspect is treated as hiding: fail closed.
        if value.len() > MAX_ATTRIBUTE_BYTES || style_hides(&decode_attribute(value)) {
            attributes.hidden = true;
        }
    } else if name.eq_ignore_ascii_case(b"href")
        && attributes.href.is_none()
        && value.len() <= MAX_ATTRIBUTE_BYTES
    {
        attributes.href = Some(decode_attribute(value));
    }
}

fn decode_attribute(value: &[u8]) -> String {
    let text = String::from_utf8_lossy(value);
    let mut decoded = String::with_capacity(text.len());
    decode_entities(&text, |c| decoded.push(c));
    decoded
}

/// Report whether an inline style hides the element, ignoring case,
/// whitespace and CSS comments. CSS escapes and style sheets are not read.
fn style_hides(style: &str) -> bool {
    let mut compact = String::with_capacity(style.len());
    let mut rest = style;
    while let Some((head, tail)) = rest.split_once("/*") {
        compact.extend(head.chars().filter(|c| !c.is_whitespace()));
        rest = tail.split_once("*/").map_or("", |(_, after)| after);
    }
    compact.extend(rest.chars().filter(|c| !c.is_whitespace()));
    let compact = compact.to_ascii_lowercase();
    compact.contains("display:none") || compact.contains("visibility:hidden")
}

/// The index after the first `needle` at or after `from`, or the end of the
/// input.
fn end_after(bytes: &[u8], from: usize, needle: &[u8]) -> usize {
    bytes
        .get(from..)
        .and_then(|rest| {
            rest.windows(needle.len())
                .position(|window| window == needle)
        })
        .map_or(bytes.len(), |position| from + position + needle.len())
}

/// The index after the first `byte` at or after `from`, or the end of the
/// input.
fn end_after_byte(bytes: &[u8], from: usize, byte: u8) -> usize {
    bytes
        .get(from..)
        .and_then(|rest| rest.iter().position(|&candidate| candidate == byte))
        .map_or(bytes.len(), |position| from + position + 1)
}

/// The index of the first ASCII case-insensitive `needle` at or after `from`.
fn find_ci(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|position| from + position)
}

/// The named references that are decoded; anything else stays literal.
const NAMED_ENTITIES: [(&str, char); 26] = [
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{a0}'),
    ("copy", '\u{a9}'),
    ("reg", '\u{ae}'),
    ("trade", '\u{2122}'),
    ("hellip", '\u{2026}'),
    ("mdash", '\u{2014}'),
    ("ndash", '\u{2013}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("ldquo", '\u{201c}'),
    ("rdquo", '\u{201d}'),
    ("bull", '\u{2022}'),
    ("middot", '\u{b7}'),
    ("times", '\u{d7}'),
    ("divide", '\u{f7}'),
    ("laquo", '\u{ab}'),
    ("raquo", '\u{bb}'),
    ("deg", '\u{b0}'),
    ("plusmn", '\u{b1}'),
    ("para", '\u{b6}'),
    ("sect", '\u{a7}'),
];

/// Decode character references in `text`, passing each character to `emit`.
///
/// Numeric references are decimal or hexadecimal. An invalid, out-of-range,
/// surrogate or control code point becomes U+FFFD. A reference without its `;`, or an unknown name,
/// stays literal.
fn decode_entities(text: &str, mut emit: impl FnMut(char)) {
    let mut rest = text;
    while let Some((head, tail)) = rest.split_once('&') {
        head.chars().for_each(&mut emit);
        match entity(tail) {
            Some((decoded, used)) => {
                emit(decoded);
                rest = tail.get(used..).unwrap_or("");
            }
            None => {
                emit('&');
                rest = tail;
            }
        }
    }
    rest.chars().for_each(&mut emit);
}

/// Decode the reference at the start of `tail` (after the `&`); returns the
/// character and the bytes used, including the `;`.
fn entity(tail: &str) -> Option<(char, usize)> {
    let end = tail.bytes().take(12).position(|byte| byte == b';')?;
    let name = tail.get(..end)?;
    let used = end + 1;
    if let Some(number) = name.strip_prefix('#') {
        let (radix, digits) = match number.strip_prefix(['x', 'X']) {
            Some(hex) => (16, hex),
            None => (10, number),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        // A number too large for u32 is out of range, so it is replaced too.
        let code = u32::from_str_radix(digits, radix).unwrap_or(u32::MAX);
        return Some((numeric_char(code), used));
    }
    NAMED_ENTITIES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, decoded)| (*decoded, used))
}

fn numeric_char(code: u32) -> char {
    let unusable =
        code == 0 || (code < 0x20 && code != 0x09 && code != 0x0a) || (0x7f..=0x9f).contains(&code);
    if unusable {
        return '\u{fffd}';
    }
    char::from_u32(code).unwrap_or('\u{fffd}')
}

#[cfg(test)]
mod tests;
