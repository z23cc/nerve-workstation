use super::*;

pub(super) struct Sources<'a, P: CatalogProvider + ?Sized> {
    provider: &'a P,
    cache: HashMap<String, Option<String>>,
}

impl<'a, P: CatalogProvider + ?Sized> Sources<'a, P> {
    pub(super) fn new(provider: &'a P) -> Self {
        Self {
            provider,
            cache: HashMap::new(),
        }
    }

    pub(super) fn source(&mut self, rel_path: &str, abs_path: &Path) -> Option<&str> {
        self.cache
            .entry(rel_path.to_string())
            .or_insert_with(|| {
                self.provider
                    .read_bytes(abs_path)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            })
            .as_deref()
    }

    /// Trimmed 1-based `line` from `rel_path`, or `None` if unavailable/blank.
    pub(super) fn line(&mut self, rel_path: &str, abs_path: &Path, line: usize) -> Option<String> {
        let source = self.source(rel_path, abs_path)?;
        let text = source.lines().nth(line.checked_sub(1)?)?.trim();
        (!text.is_empty()).then(|| text.to_string())
    }
}
