//! Ordered collection of [`DecoderProvider`]s: the single place that decides which decoder
//! opens a given file.

use std::path::Path;

use super::{AudioDecoder, DecodeError, DecoderProvider, FlacDecoderProvider};

/// Ordered list of providers; the first that [`DecoderProvider::can_open`] wins.
///
/// New formats are added by registering another provider, without touching the existing ones.
pub struct DecoderRegistry {
    providers: Vec<Box<dyn DecoderProvider>>,
}

impl DecoderRegistry {
    /// Registry with the formats supported today: FLAC only.
    pub fn with_default_providers() -> Self {
        Self::from_providers(vec![Box::new(FlacDecoderProvider)])
    }

    /// Registry with exactly these providers, tried in order.
    pub fn from_providers(providers: Vec<Box<dyn DecoderProvider>>) -> Self {
        Self { providers }
    }

    /// Opens `path` with the first provider that recognizes it.
    ///
    /// # Errors
    /// [`DecodeError::UnsupportedFileType`] when no provider recognizes the file; otherwise the
    /// error of the chosen provider.
    pub fn open(&self, path: &Path) -> Result<Box<dyn AudioDecoder>, DecodeError> {
        self.providers
            .iter()
            .find(|provider| provider.can_open(path))
            .ok_or(DecodeError::UnsupportedFileType)?
            .open(path)
    }
}

impl Default for DecoderRegistry {
    fn default() -> Self {
        Self::with_default_providers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_extension_is_unsupported() {
        let registry = DecoderRegistry::with_default_providers();
        let result = registry.open(Path::new("song.mp3"));
        assert!(matches!(result, Err(DecodeError::UnsupportedFileType)));
    }

    #[test]
    fn missing_flac_file_is_an_io_error() {
        let registry = DecoderRegistry::with_default_providers();
        let result = registry.open(Path::new("definitely-missing-file.flac"));
        assert!(matches!(result, Err(DecodeError::Io { .. })));
    }

    #[test]
    fn empty_registry_supports_nothing() {
        let registry = DecoderRegistry::from_providers(Vec::new());
        let result = registry.open(Path::new("song.flac"));
        assert!(matches!(result, Err(DecodeError::UnsupportedFileType)));
    }
}
