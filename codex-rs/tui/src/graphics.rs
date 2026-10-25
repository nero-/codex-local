//! Inline terminal graphics for the transcript.
//!
//! Image cells reserve a marker line plus blank rows. After the normal frame is drawn, this
//! module scans the visible buffer for those markers and renders the image into the reserved
//! area with `ratatui-image`. Kitty terminals use the Kitty graphics protocol (including
//! placeholder cells), Sixel terminals use Sixel, and everything else falls back to Unicode
//! halfblocks. Scrolling and resize redraws therefore participate in the normal ratatui diff.

use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;
use ratatui_image::Image;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::picker::ProtocolType;
use ratatui_image::protocol::Protocol;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;

const MARKER_START: &str = "\u{e000}";
const MARKER_END: &str = "\u{e001}";
const MAX_COLUMNS: u16 = 80;
const MAX_ROWS: u16 = 20;

#[derive(Debug, Clone)]
pub(crate) struct ImageSpec {
    pub id: u64,
    pub path: PathBuf,
    pub columns: u16,
    pub rows: u16,
}

static IMAGES: LazyLock<Mutex<HashMap<u64, ImageSpec>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Reserve a transcript slot for an image. Returns `None` when the file is not a readable image.
pub(crate) fn register_image(path: &Path) -> Option<ImageSpec> {
    let (width, height) = image::ImageReader::open(path)
        .ok()?
        .into_dimensions()
        .ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let (columns, rows) = image_cells(width, height);
    let mut id = image_id(path, columns, rows);
    let mut images = IMAGES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while images.contains_key(&id) {
        id = id.wrapping_add(1);
    }
    let spec = ImageSpec {
        id,
        path: path.to_path_buf(),
        columns,
        rows,
    };
    images.insert(id, spec.clone());
    Some(spec)
}

pub(crate) fn forget_image(id: u64) {
    IMAGES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id);
}

pub(crate) fn marker(spec: &ImageSpec) -> String {
    format!("{MARKER_START}{}{MARKER_END}", spec.id)
}

fn image_cells(width: u32, height: u32) -> (u16, u16) {
    let width = u64::from(width);
    let height = u64::from(height);
    // A terminal cell is approximately 10px wide and twice as tall as it is wide.
    // Do not upscale small images beyond their natural column count.
    let mut columns = width.div_ceil(10).clamp(1, u64::from(MAX_COLUMNS));
    let mut rows = (columns * height).div_ceil(width * 2);
    if rows > u64::from(MAX_ROWS) {
        rows = u64::from(MAX_ROWS);
        columns = (rows * 2 * width / height).clamp(1, u64::from(MAX_COLUMNS));
    }
    (columns as u16, rows.max(1) as u16)
}

fn image_id(path: &Path, columns: u16, rows: u16) -> u64 {
    use std::hash::Hash;
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    columns.hash(&mut hasher);
    rows.hash(&mut hasher);
    hasher.finish()
}

pub(crate) struct GraphicsState {
    picker: Picker,
    protocols: HashMap<(u64, u16, u16), Protocol>,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphicsState {
    pub(crate) fn new() -> Self {
        let info = terminal_info();
        let kitty = matches!(
            info.name,
            TerminalName::Kitty | TerminalName::Ghostty | TerminalName::WezTerm
        ) || info
            .term
            .as_deref()
            .is_some_and(|term| term.to_ascii_lowercase().contains("kitty"))
            || std::env::var_os("KITTY_WINDOW_ID").is_some()
            || std::env::var_os("WEZTERM_EXECUTABLE").is_some();
        // Use env/terminal detection instead of querying stdin here: the TUI event reader may
        // already own the terminal, and a missed query would delay startup for no benefit.
        // The 10x20 cell estimate still lets Kitty scale the image into the reserved cells.
        let mut picker = Picker::halfblocks();
        if kitty {
            picker.set_protocol_type(ProtocolType::Kitty);
        } else if info
            .term
            .as_deref()
            .is_some_and(|term| term.to_ascii_lowercase().contains("sixel"))
            || info
                .term
                .as_deref()
                .is_some_and(|term| term.to_ascii_lowercase().contains("foot"))
        {
            picker.set_protocol_type(ProtocolType::Sixel);
        }
        Self {
            picker,
            protocols: HashMap::new(),
        }
    }

    pub(crate) fn render(&mut self, frame: &mut crate::custom_terminal::Frame) {
        let area = frame.area();
        self.render_buffer(area, frame.buffer_mut());
    }

    fn render_buffer(&mut self, area: Rect, buffer: &mut ratatui::buffer::Buffer) {
        let text_sizing =
            crate::markdown_render::preferences::current().text_sizing && supports_text_sizing();
        if text_sizing {
            render_text_sizing(area, buffer);
        }
        let mut markers = Vec::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let Some(cell) = buffer.cell((x, y)) else {
                    continue;
                };
                let symbol = cell.symbol();
                if !symbol.starts_with(MARKER_START) {
                    continue;
                }
                let mut id = symbol.trim_start_matches(MARKER_START).to_string();
                if let Some(end) = id.find(MARKER_END) {
                    id.truncate(end);
                } else {
                    for xx in (x + 1)..area.right() {
                        let Some(cell) = buffer.cell((xx, y)) else {
                            break;
                        };
                        if let Some(end) = cell.symbol().find(MARKER_END) {
                            id.push_str(&cell.symbol()[..end]);
                            break;
                        }
                        id.push_str(cell.symbol());
                    }
                }
                if let Ok(id) = id.parse::<u64>() {
                    markers.push((id, x, y));
                }
            }
        }
        for (id, x, y) in markers {
            let Some(spec) = IMAGES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&id)
                .cloned()
            else {
                mark_unavailable(buffer, x, y, area);
                continue;
            };
            let width = spec.columns.min(area.right().saturating_sub(x));
            let height = spec.rows.min(area.bottom().saturating_sub(y));
            if width == 0 || height == 0 {
                mark_unavailable(buffer, x, y, area);
                continue;
            }
            if !self.protocols.contains_key(&(id, width, height)) {
                let Ok(reader) = image::ImageReader::open(&spec.path) else {
                    mark_unavailable(buffer, x, y, area);
                    continue;
                };
                let Ok(image) = reader.decode() else {
                    mark_unavailable(buffer, x, y, area);
                    continue;
                };
                let Ok(protocol) =
                    self.picker
                        .new_protocol(image, Size::new(width, height), Resize::Fit(None))
                else {
                    mark_unavailable(buffer, x, y, area);
                    continue;
                };
                self.protocols.insert((id, width, height), protocol);
            }
            if let Some(protocol) = self.protocols.get(&(id, width, height)) {
                Image::new(protocol)
                    .allow_clipping(true)
                    .render(Rect::new(x, y, width, height), buffer);
                clear_row_from(buffer, x.saturating_add(width), y, area.right());
            }
        }
    }
}

/// Kitty's OSC 66 text-sizing protocol wraps text in a zero-width escape. Ratatui
/// writes the cell symbol verbatim, so a heading row can carry the sequence in its
/// first cell while the layout still reserves the original row.
fn render_text_sizing(area: Rect, buffer: &mut ratatui::buffer::Buffer) {
    for y in area.top()..area.bottom() {
        // Kitty's 2x block occupies two terminal rows. Only rewrite an H1 when the
        // following row is blank (Markdown already leaves one after a heading) and
        // both rows are inside the viewport.
        if y.saturating_add(1) >= area.bottom() || !row_is_blank(buffer, y + 1, area) {
            continue;
        }
        for x in area.left()..area.right() {
            let Some(first) = buffer.cell((x, y)) else {
                continue;
            };
            if first.symbol() != "#"
                || !first
                    .modifier
                    .contains(Modifier::BOLD | Modifier::UNDERLINED)
            {
                continue;
            }
            let Some(space) = buffer.cell((x.saturating_add(1), y)) else {
                continue;
            };
            if space.symbol() != " " {
                continue;
            }
            let max_cells = u32::from(area.right().saturating_sub(x) / 2).max(1);
            let mut text = String::new();
            let mut skip_continuations = 0;
            for xx in (x.saturating_add(2))..area.right() {
                let Some(cell) = buffer.cell((xx, y)) else {
                    break;
                };
                if skip_continuations > 0 {
                    skip_continuations -= 1;
                    continue;
                }
                let symbol = cell.symbol();
                let width = unicode_width::UnicodeWidthStr::width(symbol);
                if width > 1 {
                    skip_continuations = width - 1;
                }
                text.push_str(symbol);
            }
            let text = text.trim_end();
            if text.is_empty() || unicode_width::UnicodeWidthStr::width(text) > max_cells as usize {
                // A heading that needs the full width stays at 1x rather than losing
                // its middle section when the 2x block would not fit.
                continue;
            }
            clear_row_from(buffer, x, y, area.right());
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(&format!("\x1b]66;s=2;{text}\x1b\\"));
            }
            break;
        }
    }
}

fn row_is_blank(buffer: &ratatui::buffer::Buffer, y: u16, area: Rect) -> bool {
    (area.left()..area.right()).all(|x| buffer.cell((x, y)).is_none_or(|cell| cell.symbol() == " "))
}

fn supports_text_sizing() -> bool {
    // Kitty implements OSC 66; Tern presents Kitty's graphics/text capabilities and
    // sets KITTY_WINDOW_ID. WezTerm/Ghostty do not render it, so do not enable it there.
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|term| term.to_ascii_lowercase().contains("kitty"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program.eq_ignore_ascii_case("tern"))
}

fn clear_row_from(buffer: &mut ratatui::buffer::Buffer, start: u16, y: u16, end: u16) {
    for x in start..end {
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_symbol(" ");
        }
    }
}

fn mark_unavailable(buffer: &mut ratatui::buffer::Buffer, x: u16, y: u16, area: Rect) {
    const TEXT: &str = "[image unavailable]";
    buffer.set_string(x, y, TEXT, ratatui::style::Style::new().dim());
    for xx in (x.saturating_add(TEXT.len() as u16))..area.right() {
        if let Some(cell) = buffer.cell_mut((xx, y)) {
            cell.set_symbol(" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_cells_fit_the_transcript_budget() {
        let (columns, rows) = image_cells(1600, 800);
        assert_eq!(columns, MAX_COLUMNS);
        assert_eq!(rows, 20);
        let (columns, rows) = image_cells(160, 80);
        assert_eq!((columns, rows), (16, 4));
        let (columns, rows) = image_cells(100, 2000);
        assert_eq!(rows, MAX_ROWS);
        assert!(columns <= MAX_COLUMNS);
    }

    #[test]
    fn marker_cell_renders_an_image_over_its_reserved_area() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("pixel.png");
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 30, 30, 255]));
        img.save(&path).expect("write png");
        let spec = register_image(&path).expect("image spec");
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 40, 10));
        let marker = marker(&spec);
        buffer.set_string(0, 2, &marker, ratatui::style::Style::default());
        let mut state = GraphicsState::new();
        state.render_buffer(Rect::new(0, 0, 40, 10), &mut buffer);
        let rendered = (2..(2 + spec.rows))
            .flat_map(|y| (0..spec.columns).map(move |x| (x, y)))
            .any(|(x, y)| {
                buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.symbol() != " " && cell.symbol() != MARKER_START)
            });
        assert!(rendered, "image cells were not written");
        forget_image(spec.id);
    }

    #[test]
    fn h1_row_becomes_an_osc66_sequence() {
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 40, 5));
        buffer.set_string(
            0,
            0,
            "# Heading",
            ratatui::style::Style::new().bold().underlined(),
        );
        buffer.set_string(0, 2, "## Not H1", ratatui::style::Style::new().bold());
        buffer.set_string(
            0,
            3,
            "# This heading is much too long for a 2x block",
            ratatui::style::Style::new().bold().underlined(),
        );
        render_text_sizing(Rect::new(0, 0, 40, 5), &mut buffer);
        let heading = buffer.cell((0, 0)).expect("heading cell").symbol();
        assert!(heading.contains("\x1b]66;s=2;Heading"), "{heading:?}");
        assert_eq!(buffer.cell((1, 0)).expect("cleared cell").symbol(), " ");
        assert_eq!(buffer.cell((0, 2)).expect("h2 cell").symbol(), "#");
        assert_eq!(buffer.cell((0, 3)).expect("long h1 cell").symbol(), "#");
    }

    #[test]
    fn markers_round_trip_an_id() {
        let spec = ImageSpec {
            id: 42,
            path: PathBuf::from("/tmp/image.png"),
            columns: 10,
            rows: 2,
        };
        assert!(marker(&spec).contains("42"));
    }
}
