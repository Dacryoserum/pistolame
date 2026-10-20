//! The "Photo de l'artiste" sheet of an artist's page: a Deezer search (the artist's name at
//! first), or an image file. The photo chosen replaces the automatic one for good.

use super::*;
use crate::artist_photo_search::PhotoSearchOutcome;

/// Pixels of a picker tile's thumbnail (88 px circles, sharp on a 2× screen).
const CHOICE_THUMBNAIL_PIXELS: u32 = 176;

/// The open picker: whose photo it changes, the pictures of the results shown, and the
/// generation of the latest request (older answers are dropped).
#[derive(Default)]
pub(super) struct ArtistPhotoPicker {
    artist: String,
    picture_urls: Vec<String>,
    generation: u64,
}

impl UiBridge {
    pub(super) fn connect_artist_photo_picker(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_begin_artist_photo_edit(move || handle.begin_artist_photo_edit());
        let handle = self.clone();
        state.on_search_artist_photos(move || handle.search_artist_photos());
        let handle = self.clone();
        state.on_choose_artist_photo(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.choose_artist_photo(index);
            }
        });
        let handle = self.clone();
        state.on_import_artist_photo(move || handle.import_artist_photo());
        let handle = self.clone();
        state.on_cancel_artist_photo_edit(move || handle.close_artist_photo_picker());
    }

    /// Closes the picker; a download still running is then ignored.
    fn close_artist_photo_picker(&self) {
        {
            let mut picker = self.artist_photo_picker.borrow_mut();
            picker.artist.clear();
            picker.picture_urls.clear();
            picker.generation += 1;
        }
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_is_artist_photo_picker_open(false);
            state.set_artist_photo_choices(ModelRc::default());
        }
    }

    /// Opens the picker for the artist whose page is shown, already searching their name.
    fn begin_artist_photo_edit(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let artist = state.get_artist_filter().to_string();
        if artist.is_empty() {
            return;
        }
        state.set_artist_photo_query(artist.as_str().into());
        state.set_is_artist_photo_picker_open(true);
        self.artist_photo_picker.borrow_mut().artist = artist;
        self.search_artist_photos();
    }

    /// Searches Deezer for the text of the picker's field.
    fn search_artist_photos(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let query = state.get_artist_photo_query().trim().to_owned();
        let Some(search) = &self.artist_photo_search else {
            state.set_artist_photo_status("La recherche en ligne n’a pas pu démarrer.".into());
            return;
        };
        if query.is_empty() {
            return;
        }
        let generation = self.next_artist_photo_generation();
        state.set_artist_photo_choices(ModelRc::default());
        state.set_artist_photo_status("Recherche…".into());
        search.search(generation, &query);
    }

    /// Downloads the picture of result `index`; it is stored once it arrives.
    fn choose_artist_photo(&self, index: usize) {
        let Some(url) = self
            .artist_photo_picker
            .borrow()
            .picture_urls
            .get(index)
            .cloned()
        else {
            return;
        };
        let (Some(search), Some(window)) = (&self.artist_photo_search, self.window.upgrade())
        else {
            return;
        };
        let generation = self.next_artist_photo_generation();
        window
            .global::<AppState>()
            .set_artist_photo_status("Téléchargement de la photo…".into());
        search.download(generation, &url);
    }

    /// Lets the user pick a JPEG or PNG file as the photo.
    fn import_artist_photo(&self) {
        if let Some((bytes, _)) = self.pick_cover_image() {
            self.save_artist_photo(&bytes);
        }
    }

    fn next_artist_photo_generation(&self) -> u64 {
        let mut picker = self.artist_photo_picker.borrow_mut();
        picker.generation += 1;
        picker.generation
    }

    /// Shows the answers of the latest request (called at every tick).
    pub(super) fn collect_artist_photo_search(&self) {
        let Some(search) = &self.artist_photo_search else {
            return;
        };
        for outcome in search.take_outcomes() {
            let latest = self.artist_photo_picker.borrow().generation;
            match outcome {
                PhotoSearchOutcome::Candidates {
                    generation,
                    candidates,
                    is_failed,
                } if generation == latest => self.show_artist_photo_choices(candidates, is_failed),
                PhotoSearchOutcome::Picture { generation, bytes } if generation == latest => {
                    match bytes {
                        Some(bytes) => self.save_artist_photo(&bytes),
                        None => self.show_artist_photo_status(
                            "Impossible de télécharger cette photo. Réessayez.",
                        ),
                    }
                }
                _ => {}
            }
        }
    }

    fn show_artist_photo_choices(
        &self,
        candidates: Vec<crate::artist_photo_search::PhotoCandidate>,
        is_failed: bool,
    ) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let choices: Vec<ArtistCard> = candidates
            .iter()
            .map(|candidate| ArtistCard {
                name: candidate.name.as_str().into(),
                detail: crate::artist_photo_search::fan_count_label(candidate.fan_count).into(),
                artwork: candidate
                    .thumbnail
                    .as_deref()
                    .and_then(|bytes| {
                        crate::artwork_cache::decode_scaled(bytes, CHOICE_THUMBNAIL_PIXELS)
                    })
                    .map(slint::Image::from_rgba8)
                    .unwrap_or_default(),
            })
            .collect();
        self.artist_photo_picker.borrow_mut().picture_urls = candidates
            .into_iter()
            .map(|candidate| candidate.picture_url)
            .collect();
        state.set_artist_photo_status(
            if is_failed {
                "Deezer ne répond pas. Vérifiez la connexion, ou importez une photo."
            } else if choices.is_empty() {
                "Aucun artiste trouvé. Essayez une autre orthographe."
            } else {
                ""
            }
            .into(),
        );
        state.set_artist_photo_choices(ModelRc::new(VecModel::from(choices)));
    }

    fn show_artist_photo_status(&self, text: &str) {
        if let Some(window) = self.window.upgrade() {
            window
                .global::<AppState>()
                .set_artist_photo_status(text.into());
        }
    }

    /// Stores `bytes` as the photo of the picker's artist and shows it everywhere.
    fn save_artist_photo(&self, bytes: &[u8]) {
        let artist = self.artist_photo_picker.borrow().artist.clone();
        if artist.is_empty() {
            return;
        }
        if self
            .with_library(|store| store.set_artist_image(&artist, Some(bytes)))
            .is_none()
        {
            self.show_artist_photo_status("La photo n’a pas pu être enregistrée.");
            return;
        }
        self.artist_photos
            .borrow_mut()
            .insert(artist.clone(), ArtworkId::of_bytes(bytes));
        self.close_artist_photo_picker();
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        self.show_artists();
        if state.get_artist_filter().as_str() == artist {
            self.show_artist_header(&state, &artist);
        }
        self.notice(format!("Photo de « {artist} » mise à jour"));
    }
}
