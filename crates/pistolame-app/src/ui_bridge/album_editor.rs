//! The "Modifier l'album" sheet: title, artists and cover overrides.

use super::*;

impl UiBridge {
    /// Opens the "Modifier l'album" sheet for album `index`.
    pub(super) fn begin_album_edit(&self, index: usize) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let albums = self.albums.borrow();
        let Some(album) = albums.get(index) else {
            return;
        };
        let state = window.global::<AppState>();
        state.set_editor_title(album.title.as_str().into());
        state.set_editor_new_artist("".into());
        state.set_editor_artwork(album.artwork.clone());
        state.set_editor_has_custom_artwork(album.has_custom_artwork);
        state.set_is_album_editor_open(true);
        *self.album_edit.borrow_mut() = Some(AlbumEditDraft {
            index,
            pending_artwork: PendingArtwork::Unchanged,
            artists: album.artists.clone(),
        });
        drop(albums);
        self.show_editor_artists();
    }

    /// Shows the draft's artists, and the track artists not yet among them.
    pub(super) fn show_editor_artists(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let draft = self.album_edit.borrow();
        let Some(draft) = draft.as_ref() else {
            return;
        };
        let albums = self.albums.borrow();
        let suggestions = albums
            .get(draft.index)
            .map(|album| {
                crate::album_artists::suggestions(
                    album.tracks.iter().map(|track| track.artist.as_str()),
                    &draft.artists,
                )
            })
            .unwrap_or_default();
        let model = |names: &[String]| {
            ModelRc::new(VecModel::from(
                names
                    .iter()
                    .map(|name| SharedString::from(name.as_str()))
                    .collect::<Vec<_>>(),
            ))
        };
        let state = window.global::<AppState>();
        state.set_editor_artists(model(&draft.artists));
        state.set_editor_suggested_artists(model(&suggestions));
    }

    /// Adds the typed artist (or each one, when separated by `;`).
    pub(super) fn editor_add_artist(&self, name: &str) {
        if let Some(draft) = self.album_edit.borrow_mut().as_mut() {
            for artist in crate::album_artists::split(name) {
                if !draft
                    .artists
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&artist))
                {
                    draft.artists.push(artist);
                }
            }
        }
        self.show_editor_artists();
    }

    /// Removes artist `index` from the draft.
    pub(super) fn editor_remove_artist(&self, index: usize) {
        if let Some(draft) = self.album_edit.borrow_mut().as_mut()
            && index < draft.artists.len()
        {
            draft.artists.remove(index);
        }
        self.show_editor_artists();
    }

    /// Lets the user pick a JPEG or PNG; applied only on "Enregistrer".
    pub(super) fn editor_choose_artwork(&self) {
        let Some((bytes, image)) = self.pick_cover_image() else {
            return;
        };
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_editor_artwork(image.clone());
            state.set_editor_has_custom_artwork(true);
        }
        if let Some(draft) = self.album_edit.borrow_mut().as_mut() {
            draft.pending_artwork = PendingArtwork::Chosen { bytes, image };
        }
    }

    /// Asks for a JPEG or PNG cover and checks it decodes and is not too large.
    pub(super) fn pick_cover_image(&self) -> Option<(Vec<u8>, slint::Image)> {
        let path = rfd::FileDialog::new()
            .add_filter("Image", &["jpg", "jpeg", "png"])
            .pick_file()?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.message(format!("Impossible de lire l’image : {error}"));
                return None;
            }
        };
        if bytes.len() as u64 > pistolame_library::MAX_COVER_BYTES {
            self.message("Cette image dépasse 16 Mo.".into());
            return None;
        }
        let Ok(image) = slint::Image::load_from_data(&bytes, None) else {
            self.message("Cette image n’est ni un JPEG ni un PNG lisible.".into());
            return None;
        };
        Some((bytes, image))
    }

    /// Shows the files' own cover again; applied only on "Enregistrer".
    pub(super) fn editor_reset_artwork(&self) {
        let Some(index) = self.album_edit.borrow().as_ref().map(|draft| draft.index) else {
            return;
        };
        let original = self
            .albums
            .borrow()
            .get(index)
            .map(|album| decode_artwork(original_artwork(&album.tracks)))
            .unwrap_or_default();
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_editor_artwork(original);
            state.set_editor_has_custom_artwork(false);
        }
        if let Some(draft) = self.album_edit.borrow_mut().as_mut() {
            draft.pending_artwork = PendingArtwork::Restored;
        }
    }

    /// Stores the edits in the library, then shows them everywhere.
    pub(super) fn save_album_edit(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let Some(draft) = self.album_edit.borrow_mut().take() else {
            self.close_album_editor();
            return;
        };
        let state = window.global::<AppState>();
        let Some((key, tag_title, tag_artist)) =
            self.albums.borrow().get(draft.index).map(|album| {
                (
                    album.key.clone(),
                    album.tag_title.clone(),
                    album.tag_artist.clone(),
                )
            })
        else {
            self.close_album_editor();
            return;
        };
        // A name still typed in the field counts, as if "Ajouter" had been pressed.
        let mut artists = draft.artists.clone();
        for typed in crate::album_artists::split(state.get_editor_new_artist().as_str()) {
            if !artists
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&typed))
            {
                artists.push(typed);
            }
        }
        let text = AlbumTextOverride {
            title: edited_value(state.get_editor_title().as_str(), &tag_title),
            artist: edited_value(
                &crate::album_artists::join_for_storage(&artists),
                &crate::album_artists::join_for_storage(&crate::album_artists::split(&tag_artist)),
            ),
        };
        let saved = self.with_library(|store| {
            store.set_album_text(&key, &text)?;
            match &draft.pending_artwork {
                PendingArtwork::Unchanged => Ok(()),
                PendingArtwork::Chosen { bytes, .. } => store.set_album_artwork(&key, bytes),
                PendingArtwork::Restored => store.clear_album_artwork(&key),
            }
        });
        if saved.is_none() {
            return;
        }
        if let Some(album) = self.albums.borrow_mut().get_mut(draft.index) {
            album.title = text.title.clone().unwrap_or(tag_title);
            album.artists =
                crate::album_artists::split(text.artist.as_deref().unwrap_or(&tag_artist));
            album.artist = crate::album_artists::display(&album.artists);
            match draft.pending_artwork {
                PendingArtwork::Unchanged => {}
                PendingArtwork::Chosen { image, .. } => {
                    album.artwork = image;
                    album.has_custom_artwork = true;
                }
                PendingArtwork::Restored => {
                    album.artwork = decode_artwork(original_artwork(&album.tracks));
                    album.has_custom_artwork = false;
                }
            }
        }
        self.close_album_editor();
        self.refresh_album(draft.index);
    }

    pub(super) fn close_album_editor(&self) {
        self.album_edit.borrow_mut().take();
        if let Some(window) = self.window.upgrade() {
            window.global::<AppState>().set_is_album_editor_open(false);
        }
    }
}
