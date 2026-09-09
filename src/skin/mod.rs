// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! Classic Winamp skins.
//!
//! A `.wsz` file contains bitmap sprite sheets and two small text files. This
//! module decodes them to RGBA textures. [`sprites`] defines source coordinates;
//! [`layout`] defines window positions. Missing files fall back to ytamp's
//! built-in plain skin. Modern `.wal` skins are unsupported.
//!
//! Where fastpotify shipped a drawn `.wsz` as its built-in skin, ytamp's
//! stand-in is generated: a flat grey skin whose sheets are sized to hold
//! every sprite in the table, so any other skin's gaps can be filled from it.

pub mod config;
pub mod font;
pub mod layout;
pub mod sprites;
pub mod zip;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use thiserror::Error;

pub use config::{Mask, PlaylistStyle, Regions, Rgb, VisColors};
pub use sprites::{Sheet, Sprite};

#[derive(Debug, Error)]
pub enum SkinError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a zip archive; a classic skin is a .wsz file or a folder of bitmaps")]
    NotAnArchive,
    #[error("{0}")]
    Archive(zip::ZipError),
    #[error("this is a modern Winamp skin, which ytamp cannot draw; it needs a classic one")]
    ModernSkin,
    #[error("no skin bitmaps were found inside")]
    Empty,
}

/// Distinct skins get distinct ids, so caches of textures can tell when the
/// skin under them has been swapped.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A decoded bitmap: RGBA, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Bitmap {
    /// Decodes a BMP or PNG, whatever the file was called: skins mislabel
    /// them. Anything unreadable is `None`, as Winamp silently skipped it.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let image = image::load_from_memory(bytes).ok()?.into_rgba8();
        Some(Self {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = 4 * (y * self.width + x) as usize;
        self.rgba[at..at + 4].try_into().ok()
    }

    /// A copy of the part of this bitmap a sprite covers, clipped to it.
    pub fn crop(&self, sprite: Sprite) -> Option<Bitmap> {
        let sprite = sprite.clipped_to(self.width, self.height)?;
        let mut rgba = Vec::with_capacity(4 * (sprite.width * sprite.height) as usize);
        for y in sprite.y..sprite.y + sprite.height {
            let start = 4 * (y * self.width + sprite.x) as usize;
            rgba.extend_from_slice(&self.rgba[start..start + 4 * sprite.width as usize]);
        }
        Some(Bitmap {
            width: sprite.width,
            height: sprite.height,
            rgba,
        })
    }
}

/// The files a skin is read from, keyed by lower-case file name.
type Files = HashMap<String, Vec<u8>>;

pub struct Skin {
    /// The file or folder name, for showing which skin is on.
    pub name: String,
    /// Distinct per loaded skin, for change detection.
    pub id: u64,
    sheets: HashMap<Sheet, Bitmap>,
    /// Whether the skin itself brought `nums_ex`: the built-in skin's
    /// stand-in sheet does not count, as it is generated, not read.
    extended_digits: bool,
    pub playlist: PlaylistStyle,
    pub vis_colors: VisColors,
    /// The windows' shapes, for skins that are not rectangles.
    pub regions: Regions,
}

impl Skin {
    /// Reads a `.wsz` file or an unpacked skin folder.
    pub fn load(path: &Path) -> Result<Self, SkinError> {
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            Self::from_dir(name, path)
        } else {
            Self::from_archive(name, &std::fs::read(path)?)
        }
    }

    /// Reads a `.wsz` archive. File names are matched without regard to
    /// case or folder, and when a name repeats the last copy wins, which
    /// is what unpacking onto Winamp's file system produced.
    pub fn from_archive(name: impl Into<String>, bytes: &[u8]) -> Result<Self, SkinError> {
        let name = name.into();
        let archive = zip::Archive::parse(bytes).map_err(|error| match error {
            zip::ZipError::NotAnArchive => SkinError::NotAnArchive,
            other => SkinError::Archive(other),
        })?;
        let mut files = Files::new();
        for entry in archive.entries() {
            if entry.is_dir() {
                continue;
            }
            let file_name = entry.file_name();
            if !wanted(&file_name) {
                continue;
            }
            match archive.read(entry) {
                Ok(bytes) => {
                    files.insert(file_name, bytes);
                }
                Err(error) => log::warn!("skin {name}: {error}"),
            }
        }
        Self::from_files(name, files)
    }

    /// Reads an unpacked skin: a folder with the bitmaps in it.
    pub fn from_dir(name: impl Into<String>, dir: &Path) -> Result<Self, SkinError> {
        let mut files = Files::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if wanted(&file_name) && entry.file_type()?.is_file() {
                files.insert(file_name, std::fs::read(entry.path())?);
            }
        }
        Self::from_files(name.into(), files)
    }

    fn from_files(name: String, files: Files) -> Result<Self, SkinError> {
        let mut sheets = HashMap::new();
        for sheet in Sheet::ALL {
            let stem = sheet.file_stem();
            let bytes = files
                .get(&format!("{stem}.bmp"))
                .or_else(|| files.get(&format!("{stem}.png")));
            let Some(bytes) = bytes else {
                continue;
            };
            match Bitmap::decode(bytes) {
                Some(bitmap) => {
                    sheets.insert(sheet, bitmap);
                }
                None => log::warn!("skin {name}: {stem} could not be decoded and is skipped"),
            }
        }
        if sheets.is_empty() {
            return Err(if files.contains_key("skin.xml") {
                SkinError::ModernSkin
            } else {
                SkinError::Empty
            });
        }
        let text = |file: &str| {
            files.get(file).map(|bytes| {
                // A skin edited on Windows carries a byte order mark, and U+FEFF is not
                // whitespace, so `trim` leaves it on the first line: `[Text]` stops being a
                // section header and the first `r,g,b` stops being a colour.
                let text = String::from_utf8_lossy(bytes);
                text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
            })
        };
        let playlist = text("pledit.txt")
            .map(|text| PlaylistStyle::parse(&text))
            .unwrap_or_default();
        let vis_colors = text("viscolor.txt")
            .map(|text| config::parse_vis_colors(&text))
            .unwrap_or(config::DEFAULT_VIS_COLORS);
        let regions = text("region.txt")
            .map(|text| config::parse_regions(&text))
            .unwrap_or_default();
        let extended_digits = sheets.contains_key(&Sheet::NumsEx);
        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            name,
            sheets,
            extended_digits,
            playlist,
            vis_colors,
            regions,
        })
    }

    /// The skin ytamp stands up when a skin of its own is missing: flat
    /// grey, with every sheet present so any other skin's gaps can be
    /// filled from it. It goes through no reader; it is drawn whole.
    pub fn builtin() -> Arc<Skin> {
        BUILTIN.clone()
    }

    /// Whether the skin brought this sheet itself.
    pub fn has(&self, sheet: Sheet) -> bool {
        self.sheets.contains_key(&sheet)
    }

    /// Whether the time display can take its blank cell and minus sign
    /// from the skin's own digits, rather than borrowing a bar of the 2.
    /// A skin that brought no `nums_ex` of its own — the built-in one
    /// generated its stand-in — does without.
    pub fn has_extended_digits(&self) -> bool {
        self.extended_digits
    }

    /// The bitmap for a sheet: the skin's own, or what stands in for a
    /// missing one. Balance borrows the volume sheet, whose middle the
    /// balance frames are cut from, as Winamp did; digits come from
    /// whichever digit sheet the skin has; anything else comes from the
    /// built-in skin.
    pub fn sheet(&self, sheet: Sheet) -> &Bitmap {
        if let Some(bitmap) = self.sheets.get(&sheet) {
            return bitmap;
        }
        let substitute = match sheet {
            Sheet::Balance => Sheet::Volume,
            Sheet::Numbers => Sheet::NumsEx,
            Sheet::NumsEx => Sheet::Numbers,
            other => other,
        };
        self.sheets
            .get(&substitute)
            .or_else(|| BUILTIN.sheets.get(&sheet))
            .expect("the built-in skin has every sheet")
    }

    /// The bitmap holding a sprite and the part of it the bitmap covers,
    /// or `None` when the sheet is too small to hold any of it. Winamp drew
    /// whatever was there and nothing where nothing was.
    pub fn sprite(&self, sprite: Sprite) -> Option<(&Bitmap, Sprite)> {
        let bitmap = self.sheet(sprite.sheet);
        let clipped = sprite.clipped_to(bitmap.width, bitmap.height)?;
        Some((bitmap, clipped))
    }
}

/// Whether a file inside a skin is one this reader looks at, so cursors,
/// readmes, and unrelated bitmaps are never inflated.
fn wanted(file_name: &str) -> bool {
    if matches!(
        file_name,
        "pledit.txt" | "viscolor.txt" | "region.txt" | "skin.xml"
    ) {
        return true;
    }
    let Some((stem, extension)) = file_name.rsplit_once('.') else {
        return false;
    };
    matches!(extension, "bmp" | "png") && Sheet::ALL.iter().any(|sheet| sheet.file_stem() == stem)
}

/// The grey the built-in skin is filled with, close to the classic skin's.
const BUILTIN_GREY: [u8; 3] = [61, 61, 61];

/// A sheet of the built-in skin: the flat grey, as large as the furthest
/// corner any sprite in the table reaches, the indexed ones included.
fn builtin_bitmap(sheet: Sheet) -> Bitmap {
    let indexed = [
        sprites::volume_frame(sprites::SLIDER_FRAMES - 1),
        sprites::balance_frame(sprites::SLIDER_FRAMES - 1),
        sprites::digit(9),
        sprites::digit_ex(9),
        sprites::glyph(2, 30),
        sprites::eq_slider_frame(sprites::EQ_SLIDER_FRAMES - 1),
    ];
    let mut width = 0;
    let mut height = 0;
    for sprite in sprites::ALL
        .iter()
        .map(|(_, sprite)| *sprite)
        .chain(indexed)
    {
        if sprite.sheet == sheet {
            width = width.max(sprite.x + sprite.width);
            height = height.max(sprite.y + sprite.height);
        }
    }
    let [r, g, b] = BUILTIN_GREY;
    Bitmap {
        width,
        height,
        rgba: (0..width * height).flat_map(|_| [r, g, b, 255]).collect(),
    }
}

/// The built-in skin, generated whole rather than read from a file.
static BUILTIN: LazyLock<Arc<Skin>> = LazyLock::new(|| {
    let mut sheets = HashMap::new();
    for sheet in Sheet::ALL {
        sheets.insert(sheet, builtin_bitmap(sheet));
    }
    Arc::new(Skin {
        name: "ytamp".to_string(),
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        sheets,
        extended_digits: false,
        playlist: PlaylistStyle::default(),
        vis_colors: config::DEFAULT_VIS_COLORS,
        regions: Regions::default(),
    })
});

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-colour PNG of the given size.
    fn png(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb(color));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn bmp(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb(color));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Bmp).unwrap();
        bytes.into_inner()
    }

    fn testdata(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name)
    }

    #[test]
    fn the_built_in_skin_holds_every_sprite_whole() {
        let skin = Skin::builtin();
        for sheet in Sheet::ALL {
            assert!(skin.has(sheet), "{} is missing", sheet.file_stem());
        }
        for (name, sprite) in sprites::ALL {
            let (_, clipped) = skin
                .sprite(*sprite)
                .unwrap_or_else(|| panic!("{name} is off the sheet"));
            assert_eq!(clipped, *sprite, "{name} is cut off");
        }
        for sprite in [
            sprites::volume_frame(27),
            sprites::balance_frame(27),
            sprites::digit(9),
            sprites::digit_ex(9),
            sprites::glyph(2, 30),
        ] {
            assert_eq!(
                skin.sprite(sprite).map(|(_, clipped)| clipped),
                Some(sprite)
            );
        }
        assert!(!skin.has_extended_digits());
        assert_eq!(skin.name, "ytamp");
    }

    #[test]
    fn the_built_in_skin_fills_every_gap() {
        // A skin of one bitmap still answers for every sheet and sprite,
        // through its own substitutes and the built-in skin.
        let archive = zip::write(&[("main.bmp", &png(275, 116, [1, 1, 1]), false)]);
        let skin = Skin::from_archive("sparse", &archive).unwrap();
        for sheet in Sheet::ALL {
            let bitmap = skin.sheet(sheet);
            assert!(
                bitmap.width > 0 && bitmap.height > 0,
                "{} is empty",
                sheet.file_stem()
            );
        }
        for (_, sprite) in sprites::ALL {
            assert!(skin.sprite(*sprite).is_some());
        }
    }

    #[test]
    fn files_are_found_regardless_of_case_or_folder() {
        let archive = zip::write(&[
            ("Some Skin/", b"", false),
            ("Some Skin/MAIN.BMP", &png(275, 116, [1, 2, 3]), true),
            ("Some Skin/PlEdit.TXT", b"[Text]\nNormal=#123456\n", false),
            ("Some Skin/VISCOLOR.txt", b"9,8,7\n", true),
            ("Some Skin/readme.txt", b"thanks for downloading", false),
        ]);
        let skin = Skin::from_archive("Some Skin", &archive).unwrap();
        assert!(skin.has(Sheet::Main));
        assert_eq!(skin.sheet(Sheet::Main).pixel(0, 0), Some([1, 2, 3, 255]));
        assert_eq!(skin.playlist.normal, [0x12, 0x34, 0x56]);
        assert_eq!(skin.vis_colors[0], [9, 8, 7]);
        assert_eq!(skin.vis_colors[1], config::DEFAULT_VIS_COLORS[1]);
    }

    #[test]
    fn a_skin_saved_with_a_byte_order_mark_is_still_read() {
        const BOM: &[u8] = b"\xef\xbb\xbf";
        let mut pledit = BOM.to_vec();
        pledit.extend_from_slice(b"[Text]\r\nNormal=#123456\r\n");
        let mut viscolor = BOM.to_vec();
        viscolor.extend_from_slice(b"9,8,7\n1,2,3\n");

        let archive = zip::write(&[
            ("main.bmp", &png(275, 116, [1, 2, 3]), false),
            ("pledit.txt", &pledit, false),
            ("viscolor.txt", &viscolor, false),
        ]);
        let skin = Skin::from_archive("bom", &archive).unwrap();

        assert_eq!(skin.playlist.normal, [0x12, 0x34, 0x56]);
        assert_eq!(skin.vis_colors[0], [9, 8, 7]);
        assert_eq!(skin.vis_colors[1], [1, 2, 3]);
    }

    #[test]
    fn the_last_copy_of_a_repeated_file_wins() {
        let archive = zip::write(&[
            ("main.bmp", &png(275, 116, [10, 0, 0]), false),
            ("nested/Main.bmp", &png(275, 116, [0, 20, 0]), false),
        ]);
        let skin = Skin::from_archive("twice", &archive).unwrap();
        assert_eq!(skin.sheet(Sheet::Main).pixel(5, 5), Some([0, 20, 0, 255]));
    }

    #[test]
    fn a_real_bmp_decodes_whatever_it_is_called() {
        let archive = zip::write(&[("cbuttons.png", &bmp(136, 36, [4, 5, 6]), true)]);
        let skin = Skin::from_archive("bmp", &archive).unwrap();
        let sheet = skin.sheet(Sheet::CButtons);
        assert_eq!((sheet.width, sheet.height), (136, 36));
        assert_eq!(sheet.pixel(135, 35), Some([4, 5, 6, 255]));
    }

    #[test]
    fn missing_sheets_come_from_the_built_in_skin_or_a_stand_in() {
        let archive = zip::write(&[
            ("main.bmp", &png(275, 116, [1, 1, 1]), false),
            ("volume.bmp", &png(68, 433, [2, 2, 2]), false),
            ("numbers.bmp", &png(99, 13, [3, 3, 3]), false),
        ]);
        let skin = Skin::from_archive("sparse", &archive).unwrap();
        assert!(!skin.has(Sheet::Balance));
        assert_eq!(skin.sheet(Sheet::Balance).pixel(0, 0), Some([2, 2, 2, 255]));
        assert!(!skin.has_extended_digits());
        assert_eq!(skin.sheet(Sheet::NumsEx).pixel(0, 0), Some([3, 3, 3, 255]));
        assert_eq!(
            skin.sheet(Sheet::CButtons).pixel(0, 0),
            Skin::builtin().sheet(Sheet::CButtons).pixel(0, 0)
        );
        assert_eq!(skin.playlist, PlaylistStyle::default());
    }

    #[test]
    fn a_sheet_too_small_for_a_sprite_gives_what_it_has() {
        let archive = zip::write(&[
            ("posbar.bmp", &png(307, 4, [1, 1, 1]), false),
            ("volume.bmp", &png(68, 419, [2, 2, 2]), false),
        ]);
        let skin = Skin::from_archive("short", &archive).unwrap();
        let (_, track) = skin.sprite(sprites::POSITION_TRACK).unwrap();
        assert_eq!((track.width, track.height), (248, 4));
        assert!(skin.sprite(sprites::VOLUME_THUMB).is_none());
        assert!(skin.sprite(sprites::volume_frame(27)).is_some());
    }

    #[test]
    fn an_unreadable_bitmap_is_skipped_not_fatal() {
        let archive = zip::write(&[
            ("main.bmp", &png(275, 116, [1, 1, 1]), false),
            ("cbuttons.bmp", b"this is not an image", false),
        ]);
        let skin = Skin::from_archive("broken", &archive).unwrap();
        assert!(!skin.has(Sheet::CButtons));
    }

    #[test]
    fn what_is_not_a_classic_skin_is_named_as_such() {
        assert!(matches!(
            Skin::from_archive("text", b"just some text"),
            Err(SkinError::NotAnArchive)
        ));
        let modern = zip::write(&[("skin.xml", b"<WinampAbstractionLayer/>", false)]);
        assert!(matches!(
            Skin::from_archive("modern", &modern),
            Err(SkinError::ModernSkin)
        ));
        let empty = zip::write(&[("readme.txt", b"nothing here", false)]);
        assert!(matches!(
            Skin::from_archive("empty", &empty),
            Err(SkinError::Empty)
        ));
    }

    #[test]
    fn a_folder_of_bitmaps_is_a_skin_too() {
        let dir = std::env::temp_dir().join(format!("ytamp-skin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("MAIN.BMP"), png(275, 116, [7, 7, 7])).unwrap();
        std::fs::write(dir.join("readme.txt"), b"a folder skin").unwrap();
        let skin = Skin::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(skin.name, dir.file_name().unwrap().to_string_lossy());
        assert_eq!(skin.sheet(Sheet::Main).pixel(1, 1), Some([7, 7, 7, 255]));
    }

    #[test]
    fn a_file_that_is_not_there_is_an_io_error() {
        let missing = Path::new("/nonexistent/skin.wsz");
        assert!(matches!(Skin::load(missing), Err(SkinError::Io(_))));
    }

    #[test]
    fn each_loaded_skin_has_its_own_id() {
        let archive = zip::write(&[("main.bmp", &png(275, 116, [1, 1, 1]), false)]);
        let first = Skin::from_archive("one", &archive).unwrap();
        let second = Skin::from_archive("two", &archive).unwrap();
        assert_ne!(first.id, second.id);
        assert_ne!(first.id, Skin::builtin().id);
    }

    /// The three real skins every build is checked against: the canonical
    /// base skin and two community skins, one packed loose and one nested
    /// in a folder, with a `nums_ex` sheet and their own colours.
    #[test]
    fn the_reference_skins_load_with_their_sheets_and_colours() {
        let skin = Skin::load(&testdata("base-2.91.wsz")).unwrap();
        assert_eq!(skin.name, "base-2.91");
        for sheet in [
            Sheet::Main,
            Sheet::CButtons,
            Sheet::TitleBar,
            Sheet::ShufRep,
            Sheet::PosBar,
            Sheet::Volume,
            Sheet::Balance,
            Sheet::MonoSter,
            Sheet::PlayPaus,
            Sheet::Numbers,
            Sheet::Text,
            Sheet::PlEdit,
            Sheet::EqMain,
            Sheet::EqEx,
        ] {
            assert!(skin.has(sheet), "base-2.91 lacks {}", sheet.file_stem());
        }
        // The base skin predates `nums_ex`; the plain digits stand in.
        assert!(!skin.has_extended_digits());
        assert_eq!(skin.playlist.normal, [0x00, 0xff, 0x00]);
        assert_eq!(skin.playlist.selected_background, [0x00, 0x00, 0xff]);
        assert_eq!(skin.vis_colors[0], [0, 0, 0]);
        assert_eq!(skin.vis_colors[1], [24, 33, 41]);
        assert_eq!(skin.vis_colors[2], [239, 49, 16]);
        // Its region.txt is documentation only: every section is comments,
        // so the windows stay rectangles.
        assert!(skin.regions.normal.is_none());
        assert!(skin.regions.shade.is_none());

        let skin = Skin::load(&testdata("TopazAmp1-2.wsz")).unwrap();
        assert_eq!(skin.name, "TopazAmp1-2");
        assert!(skin.has(Sheet::NumsEx));
        assert!(skin.has_extended_digits());
        assert_eq!(skin.playlist.normal, [0x31, 0x95, 0x93]);
        assert_eq!(skin.playlist.current, [0x89, 0xd8, 0xd1]);
        assert_eq!(skin.playlist.selected_background, [0x2b, 0x42, 0x42]);
        assert_eq!(skin.vis_colors[0], [28, 57, 54]);
        assert_eq!(skin.vis_colors[2], [221, 200, 156]);
        assert!(skin.regions.normal.is_none());

        let skin = Skin::load(&testdata("XMMS-Turquoise.wsz")).unwrap();
        assert_eq!(skin.name, "XMMS-Turquoise");
        assert!(skin.has(Sheet::NumsEx));
        assert_eq!(skin.playlist.normal, [0x20, 0xff, 0xf4]);
        assert_eq!(skin.playlist.normal_background, [0x08, 0x10, 0x08]);
        assert_eq!(skin.vis_colors[0], [0, 0, 0]);
        assert_eq!(skin.vis_colors[2], [10, 164, 151]);
        assert!(skin.regions.normal.is_none());
    }

    /// Every sprite of the reference skins decodes: the sheets are wide
    /// enough to hold the parts the interface paints from them.
    #[test]
    fn every_sprite_of_the_reference_skins_decodes() {
        for name in ["TopazAmp1-2.wsz", "XMMS-Turquoise.wsz"] {
            let skin = Skin::load(&testdata(name)).unwrap();
            for (name, sprite) in sprites::ALL {
                let clipped = skin
                    .sprite(*sprite)
                    .map(|(_, clipped)| clipped)
                    .unwrap_or_else(|| panic!("{name} is off its sheet"));
                assert_eq!(clipped, *sprite, "{name} is cut off");
            }
            for sprite in [
                sprites::volume_frame(27),
                sprites::balance_frame(27),
                sprites::eq_slider_frame(27),
                sprites::digit_ex(9),
                sprites::glyph(2, 30),
            ] {
                assert_eq!(
                    skin.sprite(sprite).map(|(_, clipped)| clipped),
                    Some(sprite)
                );
            }
        }
    }

    /// The bitmap font loads and has ink in it: an A is not a blank cell.
    #[test]
    fn the_font_bitmap_loads_with_ink() {
        for name in ["base-2.91.wsz", "TopazAmp1-2.wsz", "XMMS-Turquoise.wsz"] {
            let skin = Skin::load(&testdata(name)).unwrap();
            let text = skin.sheet(Sheet::Text);
            assert!(
                text.width >= 155 && text.height >= 18,
                "{name}'s text.bmp is undersized"
            );
            let a = text.crop(font::glyph('A')).unwrap();
            let blank = text.crop(font::glyph(' ')).unwrap();
            assert_ne!(a, blank, "{name}'s font has no A");
            let distinct: std::collections::HashSet<[u8; 4]> = (0..a.height)
                .flat_map(|y| (0..a.width).map(move |x| (x, y)))
                .filter_map(|(x, y)| a.pixel(x, y))
                .collect();
            assert!(distinct.len() >= 2, "{name}'s A is a flat colour");
        }
    }

    /// Loads every skin in `$YTAMP_SKIN_SAMPLES`, when set, to check the
    /// reader against real files without shipping any.
    #[test]
    fn sample_skins_load() {
        let Ok(dir) = std::env::var("YTAMP_SKIN_SAMPLES") else {
            return;
        };
        let mut seen = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "wsz") {
                continue;
            }
            let skin =
                Skin::load(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert!(
                skin.has(Sheet::Main),
                "{} has no main window",
                path.display()
            );
            for (_, sprite) in sprites::ALL {
                let _ = skin.sprite(*sprite);
            }
            seen += 1;
        }
        assert!(seen > 0, "no .wsz files in the samples folder");
    }
}
