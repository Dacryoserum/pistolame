//! The "Renommer l'artiste" sheet of an artist's page: renames the artist on every album, or
//! merges two spellings of one artist. Stored as album artist overrides; files are untouched.

use super::*;
use crate::navigation::Page;

impl UiBridge {
    pub(super) fn connect_artist_editor(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_begin_artist_edit(move || handle.begin_artist_edit());
        let handle = self.clone();
        state.on_save_artist_edit(move || handle.save_artist_edit());
    }

    /// Opens the sheet for the artist whose page is shown.
    fn begin_artist_edit(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let artist = state.get_artist_filter().to_string();
        if artist.is_empty() {
            return;
        }
        let namesake = {
            let albums = self.albums.borrow();
            crate::album_artists::namesake(
                &artist,
                albums
                    .iter()
                    .flat_map(|album| album.artists.iter().map(String::as_str)),
            )
            .unwrap_or_default()
            .to_owned()
        };
        state.set_artist_editor_name(artist.as_str().into());
        state.set_artist_editor_namesake(namesake.into());
        state.set_is_artist_editor_open(true);
    }

    /// Gives the new name to every album of the artist, then shows the renamed artist's page.
    fn save_artist_edit(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let old_name = state.get_artist_filter().to_string();
        let new_name = state.get_artist_editor_name().trim().to_owned();
        if new_name.contains(';') {
            // `;` separates the artists of an album in the stored override.
            self.message("Un nom d’artiste ne peut pas contenir « ; ».".into());
            return;
        }
        state.set_is_artist_editor_open(false);
        if new_name.is_empty() || new_name == old_name {
            return;
        }
        let edits: Vec<(AlbumKey, AlbumTextOverride)> = self
            .albums
            .borrow()
            .iter()
            .filter(|album| album.artists.contains(&old_name))
            .map(|album| {
                let artists = crate::album_artists::renamed(&album.artists, &old_name, &new_name);
                let text = AlbumTextOverride {
                    title: edited_value(&album.title, &album.tag_title),
                    artist: edited_value(
                        &crate::album_artists::join_for_storage(&artists),
                        &crate::album_artists::join_for_storage(&crate::album_artists::split(
                            &album.tag_artist,
                        )),
                    ),
                };
                (album.key.clone(), text)
            })
            .collect();
        let Some(snapshot) = self.with_library(|store| {
            for (key, text) in &edits {
                store.set_album_text(key, text)?;
            }
            store.snapshot()
        }) else {
            return;
        };
        // Before the albums change: the page of the old name would no longer exist.
        self.navigation.borrow_mut().replace_current(Page::Albums {
            artist_filter: Some(new_name.clone()),
            query: String::new(),
        });
        state.set_artist_filter(new_name.as_str().into());
        self.replace_albums(snapshot);
        self.look_up_artist_photos();
        self.notice(format!("« {old_name} » renommé en « {new_name} »"));
    }
}
