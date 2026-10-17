//! Covers loaded on demand, at the size the screen draws them.
//!
//! The library hands out cover ids, never bytes, and nothing is decoded at start-up. A cover
//! is decoded only when something on screen asks for it, from the first of:
//! 1. memory: the covers drawn recently, up to [`MEMORY_BUDGET_BYTES`] (least recently used
//!    first out);
//! 2. disk: one small JPEG per cover and size beside the library, `thumbnails/{id}-{pixels}.jpg`;
//! 3. the library database: the original bytes, decoded, scaled down, then written to disk.
//!
//! Steps 2 and 3 run on worker threads, newest request first: when scrolling, the tiles on
//! screen now matter more than those already passed. Ids are content hashes, so nothing here is
//! ever invalidated (a changed cover is a new id).
//!
//! Before this, every cover of the library was held in memory twice (encoded bytes, then
//! 512-pixel RGBA thumbnails of 1 MB each): 428 MB for 130 albums, growing with the library.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex};

use crossbeam_channel::{Receiver, Sender, unbounded};
use pistolame_library::{ArtworkId, LibraryStore};
use slint::{Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel, SharedPixelBuffer};

/// Memory kept for covers drawn recently. About 100 grid tiles at 1.5× (384 pixels), more than
/// two screens of the largest grid; covers still on screen stay alive anyway, through Slint.
const MEMORY_BUDGET_BYTES: usize = 64 * 1024 * 1024;
/// Covers kept whatever the budget: more than any screen shows (a portrait 4K screen at 2×
/// shows about 80 tiles). Slint reads every visible row again when one cover arrives, so
/// evicting a cover still on screen would request it again at once, endlessly.
const MINIMUM_KEPT_COVERS: usize = 160;
/// Largest cover drawn, in points: the album page (240); the grid draws 160 to 230.
const LARGEST_COVER_POINTS: f32 = 240.0;
/// Thumbnail sizes are multiples of this, so a small change of screen scale reuses the files.
const THUMBNAIL_PIXEL_STEP: u32 = 64;
/// Smallest and largest thumbnail edge, in pixels: 256 at 1×, 512 at 2× and above.
const THUMBNAIL_PIXEL_RANGE: (u32, u32) = (256, 512);
/// Requests waiting for a worker beyond which the oldest are dropped: a fast scroll through a
/// large library must not leave thousands of covers to decode that are no longer on screen. A
/// dropped request is simply asked again when its row comes back into view.
const MAXIMUM_PENDING_REQUESTS: usize = 128;
/// Worker threads: two keep the screen filling while one decodes a large original.
const WORKER_COUNT: usize = 2;
/// JPEG quality of the stored thumbnails: no visible loss at this size, about 40 KB each.
const STORED_THUMBNAIL_JPEG_QUALITY: u8 = 90;
const STORED_THUMBNAIL_EXTENSION: &str = "jpg";
/// The folder of stored thumbnails, beside the library database.
const THUMBNAIL_FOLDER_NAME: &str = "thumbnails";

/// Decoded pixels, shareable between threads; turned into a `slint::Image` on the UI thread.
pub(crate) type Thumbnail = SharedPixelBuffer<Rgba8Pixel>;

/// What waits for one cover: the code to run once it is in memory, or with `None` when its
/// request was dropped from a full queue (a row still on screen then asks again).
type ReadyCallback = Box<dyn FnOnce(Option<&slint::Image>)>;

thread_local! {
    /// The cache of the UI thread, for the workers' wake-up (`invoke_from_event_loop` takes a
    /// `Send` closure, and the cache is not `Send`).
    static UI_THREAD_CACHE: RefCell<Weak<ArtworkCache>> = const { RefCell::new(Weak::new()) };
}

/// The covers of the UI thread. See the module documentation.
pub(crate) struct ArtworkCache {
    /// Edge length of every thumbnail, fixed at the first request (from the screen scale) so
    /// that one cover is never decoded at two sizes in a session.
    thumbnail_pixels: Cell<Option<u32>>,
    scale_factor: Box<dyn Fn() -> Option<f32>>,
    recent: RefCell<RecentImages>,
    /// Covers that cannot be read (missing or undecodable): never asked for again.
    unavailable: RefCell<HashSet<ArtworkId>>,
    /// Covers asked for and not loaded yet, with what to do once they are.
    waiting: RefCell<HashMap<ArtworkId, Vec<ReadyCallback>>>,
    loader: Option<ThumbnailLoader>,
}

impl ArtworkCache {
    /// Starts the cache of the UI thread, reading covers from the library at `database`
    /// (`None`: no library, so no covers). `scale_factor` tells the screen scale once the
    /// window is shown.
    pub(crate) fn start(
        database: Option<PathBuf>,
        scale_factor: impl Fn() -> Option<f32> + 'static,
    ) -> Rc<ArtworkCache> {
        let wake_ui_thread = || {
            // Fails only once the event loop is gone: nobody is waiting for covers then.
            let _ = slint::invoke_from_event_loop(deliver_loaded_on_ui_thread);
        };
        let cache = Rc::new(ArtworkCache::with_loader(
            database.and_then(|database| ThumbnailLoader::start(database, wake_ui_thread)),
            Box::new(scale_factor),
        ));
        UI_THREAD_CACHE.with(|registered| *registered.borrow_mut() = Rc::downgrade(&cache));
        cache
    }

    fn with_loader(
        loader: Option<ThumbnailLoader>,
        scale_factor: Box<dyn Fn() -> Option<f32>>,
    ) -> ArtworkCache {
        ArtworkCache {
            thumbnail_pixels: Cell::new(None),
            scale_factor,
            recent: RefCell::new(RecentImages::with_budget(
                MEMORY_BUDGET_BYTES,
                MINIMUM_KEPT_COVERS,
            )),
            unavailable: RefCell::default(),
            waiting: RefCell::default(),
            loader,
        }
    }

    /// The edge length of the thumbnails, in pixels.
    pub(crate) fn thumbnail_pixels(&self) -> u32 {
        if let Some(pixels) = self.thumbnail_pixels.get() {
            return pixels;
        }
        let pixels = thumbnail_pixels_for_scale((self.scale_factor)().unwrap_or(1.0));
        self.thumbnail_pixels.set(Some(pixels));
        pixels
    }

    /// The cover, if it is in memory now.
    pub(crate) fn cached(&self, artwork: &ArtworkId) -> Option<slint::Image> {
        self.recent.borrow_mut().get(artwork)
    }

    /// Runs `on_ready` with the cover: at once when it is in memory, otherwise once a worker
    /// has loaded it (never, if it cannot be read). It runs with `None` when the request was
    /// dropped from a full queue, so that what is still on screen can ask again.
    pub(crate) fn with_image(
        &self,
        artwork: &ArtworkId,
        on_ready: impl FnOnce(Option<&slint::Image>) + 'static,
    ) {
        if let Some(image) = self.cached(artwork) {
            on_ready(Some(&image));
            return;
        }
        let Some(loader) = &self.loader else {
            return;
        };
        if self.unavailable.borrow().contains(artwork) {
            return;
        }
        let is_first_request = {
            let mut waiting = self.waiting.borrow_mut();
            let callbacks = waiting.entry(artwork.clone()).or_default();
            callbacks.push(Box::new(on_ready));
            callbacks.len() == 1
        };
        if is_first_request {
            loader.request(ThumbnailRequest {
                artwork: artwork.clone(),
                pixels: self.thumbnail_pixels(),
            });
        }
    }

    /// Shows `artwork` through `apply`, which sets one image property (the LCD, the album
    /// page...). The property is cleared at once if the cover is not in memory, then set when
    /// it arrives, unless `slot` was given another cover meanwhile.
    pub(crate) fn show(
        &self,
        slot: &ArtworkSlot,
        artwork: Option<&ArtworkId>,
        apply: impl Fn(slint::Image) + 'static,
    ) {
        let generation = slot.generation.get().wrapping_add(1);
        slot.generation.set(generation);
        let Some(artwork) = artwork else {
            apply(slint::Image::default());
            return;
        };
        if let Some(image) = self.cached(artwork) {
            apply(image);
            return;
        }
        apply(slint::Image::default());
        self.show_when_loaded(
            artwork.clone(),
            Rc::clone(&slot.generation),
            generation,
            Rc::new(apply),
        );
    }

    /// Applies `artwork` once loaded if the slot still shows it (`generation`), asking again
    /// when its request was dropped from a full queue.
    fn show_when_loaded(
        &self,
        artwork: ArtworkId,
        current_generation: Rc<Cell<u64>>,
        generation: u64,
        apply: Rc<dyn Fn(slint::Image)>,
    ) {
        self.with_image(&artwork.clone(), move |image| {
            if current_generation.get() != generation {
                return;
            }
            match image {
                Some(image) => apply(image.clone()),
                None => {
                    let cache = UI_THREAD_CACHE.with(|registered| registered.borrow().upgrade());
                    if let Some(cache) = cache {
                        cache.show_when_loaded(artwork, current_generation, generation, apply);
                    }
                }
            }
        });
    }

    /// Takes in what the workers loaded and runs what waited for it. Callbacks run with no
    /// borrow held, so they may use the cache again.
    fn deliver_loaded(&self) {
        let Some(loader) = &self.loader else {
            return;
        };
        for loaded in loader.loaded.try_iter() {
            let callbacks = self
                .waiting
                .borrow_mut()
                .remove(&loaded.artwork)
                .unwrap_or_default();
            match loaded.outcome {
                LoadOutcome::Loaded(thumbnail) => {
                    let image = slint::Image::from_rgba8(thumbnail);
                    self.recent
                        .borrow_mut()
                        .insert(loaded.artwork, image.clone());
                    for callback in callbacks {
                        callback(Some(&image));
                    }
                }
                LoadOutcome::Unreadable => {
                    self.unavailable.borrow_mut().insert(loaded.artwork);
                }
                // Rows still on screen are told, read again and ask again; without this a row
                // that asked while the request waited would stay blank.
                LoadOutcome::Dropped => {
                    for callback in callbacks {
                        callback(None);
                    }
                }
                // Asked again when its row is drawn again (telling it now could spin while the
                // library stays unreadable).
                LoadOutcome::Failed => {}
            }
        }
    }

    /// Test helper: waits until a worker delivers something, then takes it in.
    #[cfg(test)]
    fn wait_and_deliver(&self) {
        if let Some(loader) = &self.loader
            && let Ok(loaded) = loader
                .loaded
                .recv_timeout(std::time::Duration::from_secs(10))
        {
            loader.resend(loaded);
            self.deliver_loaded();
        }
    }
}

fn deliver_loaded_on_ui_thread() {
    let cache = UI_THREAD_CACHE.with(|registered| registered.borrow().upgrade());
    if let Some(cache) = cache {
        cache.deliver_loaded();
    }
}

/// One image property showing one cover at a time; see [`ArtworkCache::show`].
#[derive(Default)]
pub(crate) struct ArtworkSlot {
    generation: Rc<Cell<u64>>,
}

impl ArtworkSlot {
    /// Other code sets the property now (a cover the user just chose): a cover still loading
    /// for it is dropped when it arrives.
    pub(crate) fn take_over(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }
}

/// The thumbnail edge for a screen scale: the largest cover drawn, in pixels, rounded up.
fn thumbnail_pixels_for_scale(scale_factor: f32) -> u32 {
    let (smallest, largest) = THUMBNAIL_PIXEL_RANGE;
    let needed = (LARGEST_COVER_POINTS * scale_factor.max(1.0)).ceil() as u32;
    needed
        .div_ceil(THUMBNAIL_PIXEL_STEP)
        .saturating_mul(THUMBNAIL_PIXEL_STEP)
        .clamp(smallest, largest)
}

/// Covers drawn recently, within a memory budget; the least recently used leave first.
struct RecentImages {
    entries: HashMap<ArtworkId, RecentImage>,
    use_counter: u64,
    total_bytes: usize,
    budget_bytes: usize,
    minimum_entries: usize,
}

struct RecentImage {
    image: slint::Image,
    bytes: usize,
    last_use: u64,
}

impl RecentImages {
    /// Keeps at most `budget_bytes`, except that the `minimum_entries` most recent covers
    /// always stay.
    fn with_budget(budget_bytes: usize, minimum_entries: usize) -> RecentImages {
        RecentImages {
            entries: HashMap::new(),
            use_counter: 0,
            total_bytes: 0,
            budget_bytes,
            minimum_entries: minimum_entries.max(1),
        }
    }

    fn get(&mut self, artwork: &ArtworkId) -> Option<slint::Image> {
        self.use_counter += 1;
        let entry = self.entries.get_mut(artwork)?;
        entry.last_use = self.use_counter;
        Some(entry.image.clone())
    }

    /// Keeps `image`, then lets the least recently used go until the budget holds or only the
    /// minimum count is left. A few hundred entries at most: a linear search
    /// for the oldest is cheaper than keeping an order.
    fn insert(&mut self, artwork: ArtworkId, image: slint::Image) {
        let size = image.size();
        let bytes = size.width as usize * size.height as usize * std::mem::size_of::<Rgba8Pixel>();
        self.use_counter += 1;
        let entry = RecentImage {
            image,
            bytes,
            last_use: self.use_counter,
        };
        if let Some(replaced) = self.entries.insert(artwork, entry) {
            self.total_bytes -= replaced.bytes;
        }
        self.total_bytes += bytes;
        while self.total_bytes > self.budget_bytes && self.entries.len() > self.minimum_entries {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_use)
                .map(|(artwork, _)| artwork.clone())
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.total_bytes -= removed.bytes;
            }
        }
    }
}

/// A list model whose rows show covers. A row's cover is asked for only when Slint reads the
/// row, which a `ListView` does for the rows on screen only, and the row is redrawn when its
/// cover arrives.
pub(crate) struct LazyArtworkModel<Row> {
    rows: Vec<(Row, Option<ArtworkId>)>,
    set_artwork: fn(&mut Row, slint::Image),
    cache: Rc<ArtworkCache>,
    notify: ModelNotify,
    myself: Weak<LazyArtworkModel<Row>>,
}

/// A model of `rows`, each with the cover `set_artwork` puts into it when drawn.
pub(crate) fn lazy_artwork_model<Row: Clone + 'static>(
    cache: &Rc<ArtworkCache>,
    rows: Vec<(Row, Option<ArtworkId>)>,
    set_artwork: fn(&mut Row, slint::Image),
) -> ModelRc<Row> {
    ModelRc::from(Rc::new_cyclic(|myself| LazyArtworkModel {
        rows,
        set_artwork,
        cache: Rc::clone(cache),
        notify: ModelNotify::default(),
        myself: myself.clone(),
    }))
}

impl<Row: Clone + 'static> Model for LazyArtworkModel<Row> {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn row_data(&self, row: usize) -> Option<Row> {
        let (data, artwork) = self.rows.get(row)?;
        let mut data = data.clone();
        if let Some(artwork) = artwork {
            match self.cache.cached(artwork) {
                Some(image) => (self.set_artwork)(&mut data, image),
                None => {
                    let model = self.myself.clone();
                    self.cache.with_image(artwork, move |_| {
                        if let Some(model) = model.upgrade() {
                            model.notify.row_changed(row);
                        }
                    });
                }
            }
        }
        Some(data)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

/// One cover to load, at one size.
struct ThumbnailRequest {
    artwork: ArtworkId,
    pixels: u32,
}

/// What a worker did with a request.
enum LoadOutcome {
    Loaded(Thumbnail),
    /// Neither a stored thumbnail nor the library could give a decodable image (or decoding
    /// it panicked): never asked again in this session.
    Unreadable,
    /// Dropped from a full queue: the rows waiting for it are told, to ask again.
    Dropped,
    /// The library could not be read this time: asked again when its row is drawn again.
    Failed,
}

struct LoadedThumbnail {
    artwork: ArtworkId,
    outcome: LoadOutcome,
}

/// The worker threads and their queues.
struct ThumbnailLoader {
    queue: Arc<RequestQueue>,
    loaded: Receiver<LoadedThumbnail>,
    /// Only used to drop requests from a full queue and, in tests, to put a result back.
    loaded_sender: Sender<LoadedThumbnail>,
}

#[derive(Default)]
struct RequestQueue {
    state: Mutex<QueueState>,
    has_work: Condvar,
}

#[derive(Default)]
struct QueueState {
    /// Newest last; workers take from the end.
    pending: Vec<ThumbnailRequest>,
    is_closed: bool,
}

impl ThumbnailLoader {
    /// Starts the workers; `None` if none could start (covers then stay blank, and nothing
    /// waits for an answer that never comes).
    fn start(
        database: PathBuf,
        wake_ui_thread: impl Fn() + Clone + Send + 'static,
    ) -> Option<Self> {
        let queue = Arc::new(RequestQueue::default());
        let (loaded_sender, loaded) = unbounded();
        let mut started_count = 0;
        for index in 0..WORKER_COUNT {
            let queue = Arc::clone(&queue);
            let sender = loaded_sender.clone();
            let wake = wake_ui_thread.clone();
            let mut sources = ThumbnailSources::new(database.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("pistolame-artwork-{index}"))
                .spawn(move || {
                    while let Some(request) = queue.next() {
                        // A panic in an image decoder must still answer, or the cover would
                        // wait forever.
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                sources.load(&request)
                            }))
                            .unwrap_or(LoadOutcome::Unreadable);
                        let loaded = LoadedThumbnail {
                            artwork: request.artwork,
                            outcome,
                        };
                        if sender.send(loaded).is_err() {
                            return;
                        }
                        wake();
                    }
                });
            if spawned.is_ok() {
                started_count += 1;
            }
        }
        (started_count > 0).then_some(ThumbnailLoader {
            queue,
            loaded,
            loaded_sender,
        })
    }

    fn request(&self, request: ThumbnailRequest) {
        let dropped = {
            let mut state = self
                .queue
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pending.push(request);
            let excess = state.pending.len().saturating_sub(MAXIMUM_PENDING_REQUESTS);
            state.pending.drain(..excess).collect::<Vec<_>>()
        };
        self.queue.has_work.notify_one();
        // Reported as dropped so their callbacks are released (on the UI thread, at the next
        // delivery) and the rows can ask again.
        for request in dropped {
            let _ = self.loaded_sender.send(LoadedThumbnail {
                artwork: request.artwork,
                outcome: LoadOutcome::Dropped,
            });
        }
    }

    #[cfg(test)]
    fn resend(&self, loaded: LoadedThumbnail) {
        let _ = self.loaded_sender.send(loaded);
    }
}

impl Drop for ThumbnailLoader {
    fn drop(&mut self) {
        self.queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_closed = true;
        self.queue.has_work.notify_all();
    }
}

impl RequestQueue {
    /// The newest request, waiting for one; `None` once the loader is gone.
    fn next(&self) -> Option<ThumbnailRequest> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if state.is_closed {
                return None;
            }
            if let Some(request) = state.pending.pop() {
                return Some(request);
            }
            state = self
                .has_work
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// Where a worker finds covers: stored thumbnails, then the library (opened at first need).
struct ThumbnailSources {
    database: PathBuf,
    folder: Option<PathBuf>,
    store: Option<LibraryStore>,
}

impl ThumbnailSources {
    fn new(database: PathBuf) -> ThumbnailSources {
        ThumbnailSources {
            folder: thumbnail_folder(&database),
            database,
            store: None,
        }
    }

    fn load(&mut self, request: &ThumbnailRequest) -> LoadOutcome {
        let stored_path = self.folder.as_ref().map(|folder| {
            folder.join(format!(
                "{}-{}.{STORED_THUMBNAIL_EXTENSION}",
                request.artwork.as_str(),
                request.pixels
            ))
        });
        // A stored file that does not decode (an interrupted write) is made again below.
        if let Some(thumbnail) = stored_path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|stored| decode_scaled(&stored, request.pixels))
        {
            return LoadOutcome::Loaded(thumbnail);
        }
        if self.store.is_none() {
            self.store = LibraryStore::open(&self.database).ok();
        }
        let Some(store) = &self.store else {
            return LoadOutcome::Failed;
        };
        let original = match store.artwork_bytes(&request.artwork) {
            Ok(Some(original)) => original,
            Ok(None) => return LoadOutcome::Unreadable,
            // Busy or failing database: worth asking again later.
            Err(_) => return LoadOutcome::Failed,
        };
        let Some(thumbnail) = decode_scaled(&original, request.pixels) else {
            return LoadOutcome::Unreadable;
        };
        if let Some(path) = stored_path {
            // Without the file the cover is decoded again next time; nothing else is lost.
            let _ = store_thumbnail(&thumbnail, &path);
        }
        LoadOutcome::Loaded(thumbnail)
    }
}

/// The folder of stored thumbnails beside the library at `database`.
pub(crate) fn thumbnail_folder(database: &Path) -> Option<PathBuf> {
    database
        .parent()
        .map(|folder| folder.join(THUMBNAIL_FOLDER_NAME))
}

/// Deletes the stored thumbnails of covers no longer in the library (`stored` holds the
/// covers it still has), and the files of older naming schemes.
pub(crate) fn remove_unused_thumbnails(folder: &Path, stored: &HashSet<ArtworkId>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let kept: HashSet<&str> = stored.iter().map(ArtworkId::as_str).collect();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_used = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.split_once('-'))
            .is_some_and(|(artwork, _pixels)| kept.contains(artwork));
        if !is_used {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Decodes `bytes` (JPEG or PNG), scaled down to fit `pixels` × `pixels` if larger.
pub(crate) fn decode_scaled(bytes: &[u8], pixels: u32) -> Option<Thumbnail> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width().max(image.height()) > pixels {
        // Each source pixel feeds one target pixel: much faster than a filtered resize on a
        // 3000-pixel original, and as smooth when shrinking this much.
        image.thumbnail(pixels, pixels)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

/// Whether `bytes` start like a JPEG or PNG image with readable dimensions (reads the header
/// only).
pub(crate) fn is_readable_image(bytes: &[u8]) -> bool {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .is_some_and(|(width, height)| width > 0 && height > 0)
}

/// Writes `thumbnail` as a JPEG file (covers are opaque; the alpha channel is dropped). The
/// file is written beside, then renamed, so a crash never leaves a truncated thumbnail that
/// would decode with a grey band and be reused for good; a leftover temporary file is removed
/// by `remove_unused_thumbnails` (its name is no cover id).
fn store_thumbnail(thumbnail: &Thumbnail, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("thumbnail path without folder or name".into());
    };
    std::fs::create_dir_all(folder)?;
    let mut temporary_name = std::ffi::OsString::from("partial-");
    temporary_name.push(name);
    let temporary_path = folder.join(temporary_name);
    let rgb: Vec<u8> = thumbnail
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|[red, green, blue, _alpha]| [*red, *green, *blue])
        .collect();
    let mut file = std::io::BufWriter::new(std::fs::File::create(&temporary_path)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, STORED_THUMBNAIL_JPEG_QUALITY)
        .encode(
            &rgb,
            thumbnail.width(),
            thumbnail.height(),
            image::ExtendedColorType::Rgb8,
        )?;
    std::io::Write::flush(&mut file)?;
    drop(file);
    std::fs::rename(&temporary_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of_size(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 100, 50, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encoded");
        bytes
    }

    fn image_of_size(width: u32, height: u32) -> slint::Image {
        slint::Image::from_rgba8(SharedPixelBuffer::new(width, height))
    }

    fn test_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "pistolame-artwork-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("test folder");
        folder
    }

    #[test]
    fn a_large_cover_is_scaled_down_keeping_its_shape() {
        let thumbnail = decode_scaled(&png_of_size(2000, 1000), 512).expect("decoded");
        assert_eq!((thumbnail.width(), thumbnail.height()), (512, 256));
    }

    #[test]
    fn a_small_cover_keeps_its_size() {
        let thumbnail = decode_scaled(&png_of_size(300, 300), 512).expect("decoded");
        assert_eq!((thumbnail.width(), thumbnail.height()), (300, 300));
    }

    #[test]
    fn unreadable_bytes_give_no_image() {
        assert!(decode_scaled(b"not an image", 256).is_none());
        assert!(!is_readable_image(b"not an image"));
        assert!(is_readable_image(&png_of_size(4, 4)));
    }

    #[test]
    fn the_thumbnail_size_follows_the_screen_scale() {
        assert_eq!(thumbnail_pixels_for_scale(1.0), 256);
        assert_eq!(thumbnail_pixels_for_scale(1.25), 320);
        assert_eq!(thumbnail_pixels_for_scale(1.5), 384);
        assert_eq!(thumbnail_pixels_for_scale(2.0), 512);
        assert_eq!(thumbnail_pixels_for_scale(3.0), 512);
        assert_eq!(thumbnail_pixels_for_scale(0.5), 256);
    }

    #[test]
    fn the_least_recently_used_covers_leave_first() {
        let one_cover = 10 * 10 * 4;
        let mut recent = RecentImages::with_budget(2 * one_cover, 1);
        let [first, second, third] = [b"1", b"2", b"3"].map(|bytes| ArtworkId::of_bytes(bytes));
        recent.insert(first.clone(), image_of_size(10, 10));
        recent.insert(second.clone(), image_of_size(10, 10));
        // Using the first makes the second the oldest.
        assert!(recent.get(&first).is_some());
        recent.insert(third.clone(), image_of_size(10, 10));
        assert!(recent.get(&first).is_some());
        assert!(recent.get(&second).is_none());
        assert!(recent.get(&third).is_some());
        assert_eq!(recent.total_bytes, 2 * one_cover);
    }

    #[test]
    fn the_minimum_count_stays_even_over_budget() {
        let mut recent = RecentImages::with_budget(1, 2);
        let [first, second, third] = [b"1", b"2", b"3"].map(|bytes| ArtworkId::of_bytes(bytes));
        recent.insert(first.clone(), image_of_size(10, 10));
        recent.insert(second.clone(), image_of_size(10, 10));
        assert!(recent.get(&first).is_some() && recent.get(&second).is_some());
        recent.insert(third.clone(), image_of_size(10, 10));
        assert_eq!(recent.entries.len(), 2);
        assert!(recent.get(&first).is_none());
    }

    #[test]
    fn a_cover_is_loaded_from_the_library_then_from_its_stored_thumbnail() {
        let folder = test_folder("load");
        let database = folder.join("library.sqlite3");
        let original = png_of_size(1024, 512);
        let mut store = LibraryStore::open(&database).expect("library");
        store
            .set_artist_image("Artiste", Some(&original))
            .expect("stored");
        let artwork = ArtworkId::of_bytes(&original);
        let mut sources = ThumbnailSources::new(database.clone());
        let request = ThumbnailRequest {
            artwork: artwork.clone(),
            pixels: 256,
        };
        let LoadOutcome::Loaded(thumbnail) = sources.load(&request) else {
            panic!("decoded from the library");
        };
        assert_eq!((thumbnail.width(), thumbnail.height()), (256, 128));
        let stored = folder
            .join(THUMBNAIL_FOLDER_NAME)
            .join(format!("{}-256.jpg", artwork.as_str()));
        assert!(stored.is_file());
        // Gone from the library, still found on disk.
        store.set_artist_image("Artiste", None).expect("cleared");
        let mut fresh_sources = ThumbnailSources::new(database);
        assert!(matches!(
            fresh_sources.load(&request),
            LoadOutcome::Loaded(_)
        ));
        assert!(
            !stored
                .with_file_name(format!("partial-{}-256.jpg", artwork.as_str()))
                .exists()
        );
        // The cleanup removes it, and files of the older naming scheme.
        let older = folder.join(THUMBNAIL_FOLDER_NAME).join("0123abcd.jpg");
        std::fs::write(&older, b"old").expect("older file");
        remove_unused_thumbnails(&folder.join(THUMBNAIL_FOLDER_NAME), &HashSet::new());
        assert!(!stored.exists());
        assert!(!older.exists());
        drop(store);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn waiting_rows_are_told_once_the_cover_arrives_and_unknown_covers_are_not_asked_twice() {
        let folder = test_folder("deliver");
        let database = folder.join("library.sqlite3");
        let original = png_of_size(64, 64);
        LibraryStore::open(&database)
            .expect("library")
            .set_artist_image("Artiste", Some(&original))
            .expect("stored");
        let cache = ArtworkCache::with_loader(
            ThumbnailLoader::start(database, || {}),
            Box::new(|| Some(1.0)),
        );
        let artwork = ArtworkId::of_bytes(&original);
        let delivered = Rc::new(Cell::new(0));
        for _ in 0..2 {
            let delivered = Rc::clone(&delivered);
            cache.with_image(&artwork, move |image| {
                assert_eq!(image.map(|image| image.size().width), Some(64));
                delivered.set(delivered.get() + 1);
            });
        }
        cache.wait_and_deliver();
        assert_eq!(delivered.get(), 2);
        assert!(cache.cached(&artwork).is_some());
        let missing = ArtworkId::of_bytes(b"never stored");
        cache.with_image(&missing, |_| panic!("no image for an unknown cover"));
        cache.wait_and_deliver();
        assert!(cache.unavailable.borrow().contains(&missing));
        assert!(cache.waiting.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn rows_waiting_for_a_dropped_request_are_told_to_ask_again_but_not_after_a_failure() {
        let folder = test_folder("dropped");
        let cache = ArtworkCache::with_loader(
            ThumbnailLoader::start(folder.join("library.sqlite3"), || {}),
            Box::new(|| Some(1.0)),
        );
        let told = Rc::new(RefCell::new(Vec::new()));
        for (name, outcome) in [
            (b"dropped", LoadOutcome::Dropped),
            (b"failed!", LoadOutcome::Failed),
        ] {
            let artwork = ArtworkId::of_bytes(name);
            let told = Rc::clone(&told);
            cache
                .waiting
                .borrow_mut()
                .entry(artwork.clone())
                .or_default()
                .push(Box::new(move |image| {
                    told.borrow_mut().push((name, image.is_none()));
                }));
            let loader = cache.loader.as_ref().expect("workers started");
            loader.resend(LoadedThumbnail { artwork, outcome });
        }
        cache.deliver_loaded();
        assert_eq!(*told.borrow(), vec![(b"dropped", true)]);
        assert!(cache.waiting.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_slot_ignores_a_cover_that_arrives_after_it_moved_on() {
        let cache = ArtworkCache::with_loader(None, Box::new(|| Some(1.0)));
        let slot = ArtworkSlot::default();
        let shown = Rc::new(RefCell::new(Vec::new()));
        let record = |shown: &Rc<RefCell<Vec<u32>>>| {
            let shown = Rc::clone(shown);
            move |image: slint::Image| shown.borrow_mut().push(image.size().width)
        };
        let cached = ArtworkId::of_bytes(b"cached");
        cache
            .recent
            .borrow_mut()
            .insert(cached.clone(), image_of_size(7, 7));
        cache.show(&slot, Some(&cached), record(&shown));
        cache.show(&slot, None, record(&shown));
        assert_eq!(*shown.borrow(), vec![7, 0]);
    }
}
